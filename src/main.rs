mod loop_step;
mod setup;
mod state;
mod util;

use loop_step::{Env, ResultPrinter, ShellCommand};
use setup::setup;
use state::{Counters, State, Summary};

use std::fs::File;
use std::process;

use regex::Regex;
use subprocess::{Exec, ExitStatus, Redirection};

fn main() {
    // Time
    let (opt, items, count_precision, program_start) = setup();

    let opt_only_last = opt.only_last;
    let opt_summary = opt.summary;

    let cmd_with_args = opt.input.join(" ");
    if cmd_with_args.is_empty() {
        println!("No command supplied, exiting.");
        return;
    }

    // Counters and State
    let env: &dyn Env = &RealEnv {};
    let shell_command: &dyn ShellCommand = &RealShellCommand {};
    let result_printer: &dyn ResultPrinter = &RealResultPrinter {
        only_last: opt.only_last,
        until_contains: opt.until_contains.clone(),
        until_match: opt.until_match.clone(),
    };

    let iterator = LoopIterator::new(opt.offset, opt.count_by, opt.num, &items);

    let loop_model = opt.into_loop_model(
        cmd_with_args,
        program_start,
        items,
        env,
        shell_command,
        result_printer,
    );

    let mut state = State::default();

    for (index, actual_count) in iterator.enumerate() {
        let counters = Counters {
            count_precision,
            index,
            actual_count,
        };

        let (break_loop, new_state) = loop_model.step(state, counters);
        state = new_state;

        if break_loop {
            break;
        }
    }

    pre_exit_tasks(opt_only_last, opt_summary, state.summary, state.tmpfile);

    process::exit(state.exit_status);
}

struct RealEnv {}

impl Env for RealEnv {
    fn set(&self, k: &str, v: &str) {
        std::env::set_var(k, v);
    }
}

struct RealShellCommand {}

impl ShellCommand for RealShellCommand {
    fn run(&self, mut state: State, cmd_with_args: &str) -> (ExitStatus, State) {
        use std::io::{prelude::*, SeekFrom};

        state.tmpfile.seek(SeekFrom::Start(0)).ok();
        state.tmpfile.set_len(0).ok();

        let status = Exec::shell(cmd_with_args)
            .stdout(Redirection::File(state.tmpfile.try_clone().unwrap()))
            .stderr(Redirection::Merge)
            .capture()
            .unwrap()
            .exit_status;

        (status, state)
    }
}

struct RealResultPrinter {
    only_last: bool,
    until_contains: Option<String>,
    until_match: Option<Regex>,
}

impl ResultPrinter for RealResultPrinter {
    fn print(&self, mut state: State, stdout: &str) -> State {
        stdout.lines().for_each(|line| {
            // --only-last
            // If we only want output from the last execution,
            // defer printing until later
            if !self.only_last {
                println!("{}", line);
            }

            // --until-contains
            // We defer loop breaking until the entire result is printed.
            if let Some(ref string) = self.until_contains {
                state.has_matched = line.contains(string);
            }

            // --until-match
            if let Some(ref regex) = self.until_match {
                state.has_matched = regex.captures(&line).is_some();
            }
        });

        state
    }
}

fn pre_exit_tasks(only_last: bool, print_summary: bool, summary: Summary, mut tmpfile: File) {
    use util::StringFromTempfileStart;

    if only_last {
        String::from_temp_start(&mut tmpfile)
            .lines()
            .for_each(|line| println!("{}", line));
    }

    if print_summary {
        summary.print()
    }
}

struct LoopIterator {
    start: f64,
    iters: f64,
    end: f64,
    step_by: f64,
}

impl LoopIterator {
    fn new(offset: f64, count_by: f64, num: Option<f64>, items: &[String]) -> LoopIterator {
        let end = if let Some(num) = num {
            num
        } else if !items.is_empty() {
            items.len() as f64
        } else {
            std::f64::INFINITY
        };
        LoopIterator {
            start: offset - count_by,
            iters: 0.0,
            end,
            step_by: count_by,
        }
    }
}

impl Iterator for LoopIterator {
    type Item = f64;

    fn next(&mut self) -> Option<Self::Item> {
        self.start += self.step_by;
        self.iters += 1.0;
        if self.iters <= self.end {
            Some(self.start)
        } else {
            None
        }
    }
}
