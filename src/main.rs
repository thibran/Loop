extern crate atty;
extern crate humantime;
extern crate regex;
extern crate structopt;
extern crate subprocess;
extern crate tempfile;

use std::env;
use std::f64;
use std::fs::File;
use std::io::prelude::*;
use std::io::{self, BufRead, SeekFrom};
use std::process;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use humantime::{parse_duration, parse_rfc3339_weak};
use regex::Regex;
use structopt::StructOpt;
use subprocess::{Exec, ExitStatus, Redirection};

static UNKONWN_EXIT_CODE: u32 = 99;

// same exit code as use of `timeout` shell command
static TIMEOUT_EXIT_CODE: i32 = 124;

fn main() {
    // Load the CLI arguments
    let opt = Opt::from_args();
    let count_precision = Opt::clap()
        .get_matches()
        .value_of("count_by")
        .map(precision_of)
        .unwrap_or(0);

    let mut exit_status = 0;

    // Time
    let program_start = Instant::now();

    // Number of iterations
    let mut items: Vec<String> = opt.ffor.clone().unwrap_or_else(|| vec![]);

    // Get any lines from stdin
    if opt.stdin || atty::isnt(atty::Stream::Stdin) {
        io::stdin()
            .lock()
            .lines()
            .for_each(|line| items.push(line.unwrap().to_owned()));
    }

    let joined_input = &opt.input.join(" ");
    if joined_input.is_empty() {
        println!("No command supplied, exiting.");
        return;
    }

    // Counters and State
    let mut has_matched = false;
    let mut tmpfile = tempfile::tempfile().unwrap();
    let mut summary = Summary::default();
    let mut previous_stdout = None;

    let counters = counters_from_opt(&opt, &items);
    for (count, actual_count) in counters.iter().enumerate() {
        // Time Start
        let loop_start = Instant::now();

        // Set counters before execution
        // THESE ARE FLIPPED AND I CAN'T UNFLIP THEM.
        env::set_var("ACTUALCOUNT", count.to_string());
        env::set_var("COUNT", format!("{:.*}", count_precision, actual_count));

        // Set iterated item as environment variable
        if let Some(item) = items.get(count) {
            env::set_var("ITEM", item);
        }

        // Finish if we're over our duration
        if let Some(duration) = opt.for_duration {
            let since = Instant::now().duration_since(program_start);
            if since >= duration {
                if opt.error_duration {
                    exit_status = TIMEOUT_EXIT_CODE
                }
                break;
            }
        }

        // Finish if our time until has passed
        // In this location, the loop will execute at least once,
        // even if the start time is beyond the until time.
        if let Some(until_time) = opt.until_time {
            if SystemTime::now().duration_since(until_time).is_ok() {
                break;
            }
        }

        // Main executor
        tmpfile.seek(SeekFrom::Start(0)).ok();
        tmpfile.set_len(0).ok();
        let result = Exec::shell(joined_input)
            .stdout(Redirection::File(tmpfile.try_clone().unwrap()))
            .stderr(Redirection::Merge)
            .capture()
            .unwrap();

        // Print the results
        let stdout = String::from_temp_start(&mut tmpfile);
        print_results(&opt, &mut has_matched, &stdout);

        // --until-error
        check_error_code(&opt.until_error, &mut has_matched, result.exit_status);

        // --until-success
        if opt.until_success && result.exit_status.success() {
            has_matched = true;
        }

        // --until-fail
        if opt.until_fail && !(result.exit_status.success()) {
            has_matched = true;
        }

        if opt.summary {
            match result.exit_status {
                ExitStatus::Exited(0) => summary.successes += 1,
                ExitStatus::Exited(n) => summary.failures.push(n),
                _ => summary.failures.push(UNKONWN_EXIT_CODE),
            }
        }

        // Finish if we matched
        if has_matched {
            break;
        }

        if let Some(ref previous_stdout) = previous_stdout {
            // --until-changes
            if opt.until_changes && *previous_stdout != stdout {
                break;
            }

            // --until-same
            if opt.until_same && *previous_stdout == stdout {
                break;
            }
        } else {
            previous_stdout = Some(stdout);
        }

        // Delay until next iteration time
        let since = Instant::now().duration_since(loop_start);
        if let Some(time) = opt.every.checked_sub(since) {
            thread::sleep(time);
        }
    }

    exit_app(opt.only_last, opt.summary, exit_status, summary, tmpfile)
}

fn counters_from_opt(opt: &Opt, items: &[String]) -> Vec<f64> {
    let mut counters = vec![];
    let mut start = opt.offset - opt.count_by;
    let mut index = 0_f64;
    let step_by = opt.count_by;
    let end = if let Some(num) = opt.num {
        num
    } else if !items.is_empty() {
        items.len() as f64
    } else {
        f64::INFINITY
    };
    loop {
        start += step_by;
        index += 1_f64;
        if index <= end {
            counters.push(start)
        } else {
            break;
        }
    }
    counters
}

fn print_results(opt: &Opt, has_matched: &mut bool, stdout: &str) {
    stdout.lines().for_each(|line| {
        // --only-last
        // If we only want output from the last execution,
        // defer printing until later
        if !opt.only_last {
            println!("{}", line);
        }

        // --until-contains
        // We defer loop breaking until the entire result is printed.
        if let Some(ref string) = opt.until_contains {
            *has_matched = line.contains(string);
        }

        // --until-match
        if let Some(ref regex) = opt.until_match {
            *has_matched = regex.captures(&line).is_some();
        }
    })
}

fn check_error_code(
    maybe_error: &Option<ErrorCode>,
    has_matched: &mut bool,
    exit_status: subprocess::ExitStatus,
) {
    match maybe_error {
        Some(ErrorCode::Any) => {
            *has_matched = !exit_status.success();
        }
        Some(ErrorCode::Code(code)) => {
            if exit_status == ExitStatus::Exited(*code) {
                *has_matched = true;
            }
        }
        _ => (),
    }
}

fn exit_app(
    only_last: bool,
    print_summary: bool,
    exit_status: i32,
    summary: Summary,
    mut tmpfile: File,
) {
    if only_last {
        String::from_temp_start(&mut tmpfile)
            .lines()
            .for_each(|line| println!("{}", line));
    }

    if print_summary {
        summary.print()
    }

    process::exit(exit_status);
}

trait StringFromTempfileStart {
    fn from_temp_start(tmpfile: &mut File) -> String;
}

impl StringFromTempfileStart for String {
    fn from_temp_start(tmpfile: &mut File) -> String {
        let mut stdout = String::new();
        tmpfile.seek(SeekFrom::Start(0)).ok();
        tmpfile.read_to_string(&mut stdout).ok();
        stdout
    }
}

#[derive(StructOpt, Debug)]
#[structopt(
    name = "loop",
    author = "Rich Jones <miserlou@gmail.com>",
    about = "UNIX's missing `loop` command"
)]
struct Opt {
    /// Number of iterations to execute
    #[structopt(short = "n", long = "num")]
    num: Option<f64>,

    /// Amount to increment the counter by
    #[structopt(short = "b", long = "count-by", default_value = "1")]
    count_by: f64,

    /// Amount to offset the initial counter by
    #[structopt(short = "o", long = "offset", default_value = "0")]
    offset: f64,

    /// How often to iterate. ex., 5s, 1h1m1s1ms1us
    #[structopt(
        short = "e",
        long = "every",
        default_value = "1us",
        parse(try_from_str = "parse_duration")
    )]
    every: Duration,

    /// A comma-separated list of values, placed into 4ITEM. ex., red,green,blue
    #[structopt(long = "for", parse(from_str = "get_values"))]
    ffor: Option<Vec<String>>,

    /// Keep going until the duration has elapsed (example 1m30s)
    #[structopt(
        short = "d",
        long = "for-duration",
        parse(try_from_str = "parse_duration")
    )]
    for_duration: Option<Duration>,

    /// Keep going until the output contains this string
    #[structopt(short = "c", long = "until-contains")]
    until_contains: Option<String>,

    /// Keep going until the output changes
    #[structopt(short = "C", long = "until-changes")]
    until_changes: bool,

    /// Keep going until the output changes
    #[structopt(short = "S", long = "until-same")]
    until_same: bool,

    /// Keep going until the output matches this regular expression
    #[structopt(short = "m", long = "until-match", parse(try_from_str = "Regex::new"))]
    until_match: Option<Regex>,

    /// Keep going until a future time, ex. "2018-04-20 04:20:00" (Times in UTC.)
    #[structopt(
        short = "t",
        long = "until-time",
        parse(try_from_str = "parse_rfc3339_weak")
    )]
    until_time: Option<SystemTime>,

    /// Keep going until the command exit status is non-zero, or the value given
    #[structopt(short = "r", long = "until-error", parse(from_str = "get_error_code"))]
    until_error: Option<ErrorCode>,

    /// Keep going until the command exit status is zero
    #[structopt(short = "s", long = "until-success")]
    until_success: bool,

    /// Keep going until the command exit status is non-zero
    #[structopt(short = "f", long = "until-fail")]
    until_fail: bool,

    /// Only print the output of the last execution of the command
    #[structopt(short = "l", long = "only-last")]
    only_last: bool,

    /// Read from standard input
    #[structopt(short = "i", long = "stdin")]
    stdin: bool,

    /// Exit with timeout error code on duration
    #[structopt(short = "D", long = "error-duration")]
    error_duration: bool,

    /// Provide a summary
    #[structopt(long = "summary")]
    summary: bool,

    /// The command to be looped
    #[structopt(raw(multiple = "true"))]
    input: Vec<String>,
}

fn precision_of(s: &str) -> usize {
    let after_point = match s.find('.') {
        // '.' is ASCII so has len 1
        Some(point) => point + 1,
        None => return 0,
    };
    let exp = match s.find(&['e', 'E'][..]) {
        Some(exp) => exp,
        None => s.len(),
    };
    exp - after_point
}

#[derive(Debug)]
enum ErrorCode {
    Any,
    Code(u32),
}

fn get_error_code(input: &&str) -> ErrorCode {
    if let Ok(code) = input.parse::<u32>() {
        ErrorCode::Code(code)
    } else {
        ErrorCode::Any
    }
}

fn get_values(input: &&str) -> Vec<String> {
    if input.contains('\n') {
        input.split('\n').map(String::from).collect()
    } else if input.contains(',') {
        input.split(',').map(String::from).collect()
    } else {
        input.split(' ').map(String::from).collect()
    }
}

#[derive(Debug)]
struct Summary {
    successes: u32,
    failures: Vec<u32>,
}

impl Summary {
    fn print(self) {
        let total = self.successes + self.failures.len() as u32;

        let errors = if self.failures.is_empty() {
            String::from("0")
        } else {
            format!(
                "{} ({})",
                self.failures.len(),
                self.failures
                    .into_iter()
                    .map(|f| (-(f as i32)).to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            )
        };

        println!("Total runs:\t{}", total);
        println!("Successes:\t{}", self.successes);
        println!("Failures:\t{}", errors);
    }
}

impl Default for Summary {
    fn default() -> Summary {
        Summary {
            successes: 0,
            failures: Vec::new(),
        }
    }
}
