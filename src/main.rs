#![cfg_attr(rustc_nightly, feature(test))]

#[cfg(test)]
extern crate byteorder;
extern crate chrono;
extern crate clap;
extern crate elf;
extern crate env_logger;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate failure_derive;
#[cfg(test)]
#[macro_use]
extern crate lazy_static;
extern crate libc;
#[macro_use]
extern crate log;
extern crate read_process_memory;
#[cfg(target_os = "macos")]
extern crate regex;
extern crate ruby_bindings as bindings;

use chrono::prelude::*;
use clap::{App, AppSettings, Arg, ArgMatches, SubCommand};
use libc::pid_t;
use failure::Error;

pub mod proc_maps;
pub mod address_finder;
pub mod initialize;
pub mod copy;
pub mod ruby_version;
pub mod test_utils;

fn do_main() -> Result<(), Error> {
    env_logger::init().unwrap();

    let matches: ArgMatches<'static> = arg_parser().get_matches();
    match matches.subcommand() {
        ("snapshot", Some(sub_m)) => {
            let pid_string = sub_m.value_of("pid").expect("Failed to find PID");
            let pid = pid_string
                .parse()
                .map_err(|_| format_err!("Invalid PID: {}", pid_string))?;
            Ok(snapshot(pid)?)
        }
        ("record", Some(sub_m)) => {
            let maybe_pid = sub_m.value_of("pid");
            let maybe_cmd = sub_m.values_of("cmd");
            let maybe_filename = sub_m.value_of("file");
            let pid: pid_t = match maybe_pid {
                Some(pid_string) => pid_string
                    .parse()
                    .map_err(|_| format_err!("Invalid PID: {}", pid_string))?,
                None => {
                    exec_cmd(&mut maybe_cmd.expect("Either PID or command is required to record"))?
                }
            };
            Ok(record(maybe_filename, pid)?)
        }
        _ => panic!("not a valid subcommand"),
    }
}

fn main() {
    match do_main() {
        Err(x) => {
            println!("Error. Causes: ");
            for c in x.causes() {
                println!("- {}", c);
            }
            println!("{}", x.backtrace());
            std::process::exit(1);
        }
        _ => {}
    }
}

fn record(filename: Option<&str>, pid: pid_t) -> Result<(), Error> {
    // This gets a stack trace and then just prints it out
    // in a format that Brendan Gregg's stackcollapse.pl script understands
    let getter = initialize::initialize(pid)?;
    let mut output = open_record_output(filename)?;
    let mut errors = 0;
    let mut successes = 0;
    let mut quit = false;
    loop {
        let trace = getter.get_trace();
        match trace {
            Err(copy::MemoryCopyError::ProcessEnded) => return Ok(()),
            Ok(ref ok_trace) => {
                successes += 1;
                for t in ok_trace.iter().rev() {
                    write!(output, "{}", t)?;
                    write!(output, ";")?;
                }
                writeln!(output, " {}", 1)?;
            }
            Err(ref x) => {
                errors += 1;
                println!(
                    "{} {}",
                    errors,
                    (errors as f64) / (errors as f64 + successes as f64)
                );
                if errors > 20 && (errors as f64) / (errors as f64 + successes as f64) > 0.5 {
                    // TODO: figure out how to just return an error here
                    quit = true;
                }
                println!("Dropping one stack trace: {:?}", x);
            }
        }
        if quit == true {
            trace?;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn snapshot(pid: pid_t) -> Result<(), Error> {
    let getter = initialize::initialize(pid)?;
    let trace = getter.get_trace()?;
    for x in trace.iter().rev() {
        println!("{}", x);
    }
    Ok(())
}

fn output_dir_name() -> Result<Box<std::path::PathBuf>, Error> {
    use std::os::unix::prelude::*;
    use std::fs;
    let home = std::env::var("HOME")?;
    let mut dirname = std::path::Path::new(&home).join(".cache");
    let dirs = vec![".rbspy", "records"];
    for dir in dirs {
        dirname = dirname.join(dir);
        if !dirname.exists() {
            // create dir with permissions 777 so that if we're running as sudo the user doesn't
            // lose access to the dir. TODO: should use chown instead
            fs::create_dir(&dirname)?;
            let permissions = std::fs::Permissions::from_mode(0o777);
            std::fs::set_permissions(&dirname, permissions)?;
        }
    }
    Ok(Box::new(dirname))
}

fn open_record_output(maybe_filename: Option<&str>) -> Result<Box<std::io::Write>, Error> {
    match maybe_filename {
        Some(filename) => {
            let path = std::path::Path::new(filename);
            println!("Recording data into {:?}...", path);
            Ok(Box::new(std::fs::File::create(path)?))
        }
        None => {
            let filename = format!("{}-{}.txt", "rbspy", Utc::now().to_rfc3339());
            let dirname = &output_dir_name()?;
            let path = dirname.join(filename);
            println!("Recording data into {:?}...", path);
            Ok(Box::new(std::fs::File::create(path)?))
        }
    }
}

fn exec_cmd(args: &mut std::iter::Iterator<Item = &str>) -> Result<pid_t, Error> {
    let arg1 = args.next().unwrap();
    let pid = std::process::Command::new(arg1).args(args).spawn()?.id() as pid_t;
    Ok(pid)
}

fn arg_parser() -> App<'static, 'static> {
    App::new("rbspy")
        .about("Sampling profiler for Ruby programs")
        .setting(AppSettings::SubcommandRequired)
        .subcommand(
            SubCommand::with_name("snapshot")
                .about("Snapshot a single stack trace")
                .arg(
                    Arg::from_usage("-p --pid=[PID] 'PID of the Ruby process you want to profile'")
                        .required_unless("cmd"),
                ),
        )
        .subcommand(
            SubCommand::with_name("record")
                .about("Record process")
                .arg(
                    Arg::from_usage("-p --pid=[PID] 'PID of the Ruby process you want to profile'")
                        .required_unless("cmd"),
                )
                .arg(Arg::from_usage("-f --file=[FILE] 'File to write output to'").required(false))
                .arg(Arg::from_usage("<cmd>... 'commands to run'").required(false)),
        )
}

#[test]
fn test_arg_parsing() {
    let parser = arg_parser();
    // let result = parser.get_matches_from(vec!("rbspy", "stackcollapse", "-p", "1234"));
    let result = parser.get_matches_from(vec!["rbspy", "record", "--pid", "1234"]);
    let result = result.subcommand_matches("record").unwrap();
    assert!(result.value_of("pid").unwrap() == "1234");

    let parser = arg_parser();
    let result = parser.get_matches_from(vec!["rbspy", "snapshot", "--pid", "1234"]);
    let result = result.subcommand_matches("snapshot").unwrap();
    assert!(result.value_of("pid").unwrap() == "1234");

    let parser = arg_parser();
    let result = parser.get_matches_from(vec!["rbspy", "record", "ruby", "blah.rb"]);
    let result = result.subcommand_matches("record").unwrap();
    let mut cmd_values = result.values_of("cmd").unwrap();
    assert!(cmd_values.next().unwrap() == "ruby");
    assert!(cmd_values.next().unwrap() == "blah.rb");
}
