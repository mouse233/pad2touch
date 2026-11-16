mod decode;
mod experiment;
mod hid;
mod injection;
mod logging;
mod scenarios;
mod touch;
mod verify;
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--verify-log") {
        if args.len() != 2 {
            return Err("usage: --verify-log PATH_TO_EVENTS_LOG".into());
        }
        let audit =
            verify::analyze(&std::fs::read_to_string(&args[1]).map_err(|e| e.to_string())?)?;
        println!("{}", audit.json());
        return if audit.passed() {
            Ok(())
        } else {
            Err("log audit found incomplete or inconsistent touch delivery".into())
        };
    }
    if args.is_empty() || args == ["--help"] {
        println!("PadBridge Phase 1+2 (hardware validation pending)\n  --probe\n  --capture [--inject] [--contacts 1..5] [--device 0] [--seconds 60] [--out NEW_DIRECTORY]\n  --self-test [--contacts 1..5] [--seconds 40] [--out NEW_DIRECTORY]\n  --verify-log PATH_TO_EVENTS_LOG\n--contacts 1: single-contact MVP; default 5: synchronized multitouch.\nSelf-test injects SYNTHETIC coordinates into a real Windows receiver; run manually.\nNo GUI or injection is started by cargo test or --verify-log. ESC releases all contacts and stops.");
        return Ok(());
    }
    if args == ["--probe"] {
        return hid::probe();
    }
    experiment::run(parse_options(&args)?)
}
fn parse_options(args: &[String]) -> Result<experiment::Options, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let mut opt = experiment::Options {
        mode: String::new(),
        inject: false,
        contacts: 5,
        device: 0,
        seconds: 60,
        out: format!("artifacts/run-{stamp}").into(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--capture" | "--self-test" => {
                if !opt.mode.is_empty() {
                    return Err("select exactly one of --capture or --self-test".into());
                }
                opt.mode = args[i].trim_start_matches("--").into();
            }
            "--inject" => opt.inject = true,
            "--device" | "--seconds" | "--out" | "--contacts" => {
                let key = &args[i];
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| format!("{key} requires a value"))?;
                match key.as_str() {
                    "--device" => opt.device = v.parse().map_err(|_| "invalid device index")?,
                    "--seconds" => opt.seconds = v.parse().map_err(|_| "invalid seconds")?,
                    "--contacts" => {
                        opt.contacts = v.parse().map_err(|_| "invalid contact capacity")?
                    }
                    _ => opt.out = v.into(),
                }
            }
            x => return Err(format!("unknown argument {x}")),
        }
        i += 1;
    }
    if opt.mode.is_empty() || opt.seconds == 0 {
        return Err("select --capture or --self-test and seconds >0".into());
    }
    if !(1..=5).contains(&opt.contacts) {
        return Err("--contacts must be in 1..=5".into());
    }
    if opt.inject && opt.mode != "capture" {
        return Err(
            "--inject is only used with --capture; --self-test injects automatically".into(),
        );
    }
    Ok(opt)
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn invalid_or_ambiguous_modes_rejected() {
        for a in [
            vec!["--capture", "--self-test"],
            vec!["--capture", "--contacts", "6"],
            vec!["--capture", "--contacts", "0"],
            vec!["--capture", "--seconds", "0"],
            vec!["--capture", "--device"],
            vec!["--self-test", "--inject"],
        ] {
            assert!(parse_options(&args(&a)).is_err());
        }
    }
    #[test]
    fn one_and_five_contacts_parse_without_starting_windows() {
        for cap in ["1", "5"] {
            let o = parse_options(&args(&[
                "--capture",
                "--inject",
                "--contacts",
                cap,
                "--seconds",
                "120",
            ]))
            .unwrap();
            assert_eq!(o.contacts, cap.parse::<usize>().unwrap());
            assert!(o.inject);
            assert_eq!(o.seconds, 120);
        }
    }
}
