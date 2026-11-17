mod background_guard;
mod capture;
mod control;
mod decode;
mod device_info;
mod experiment;
mod gestures;
mod gui;
mod hid;
mod injection;
mod logging;
mod mapping;
mod render;
mod report_rate;
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
    if args.is_empty() || args == ["--gui"] {
        return gui::run();
    }
    if args == ["--gesture-support"] {
        println!(
            "foreground_touchpad_gesture_api_supported={}",
            gestures::probe()?
        );
        return Ok(());
    }
    if args == ["--gesture-check"] {
        return gestures::check_native_controller();
    }
    if args.first().is_some_and(|s| s == "--verify-log") {
        if args.len() != 2 {
            return Err("usage: --verify-log PATH_TO_EVENTS_LOG".into());
        }
        let audit =
            verify::analyze(&std::fs::read_to_string(&args[1]).map_err(|e| e.to_string())?)?;
        println!("{}", audit.json());
        // Expected audit failures are already explained by JSON on stdout.
        // Keep stderr for command/parse errors, which also helps PowerShell 5.
        if !audit.passed() {
            std::process::exit(1);
        }
        return Ok(());
    }
    if args == ["--help"] {
        println!("PadBridge (multitouch hardware validation pending)\n  --gui (default: integrated diagnostics, devices, manual / toggle / hold)\n  --probe\n  --gesture-support (read-only capability check)\n  --gesture-check (brief native controller activation/readback/release)\n  --capture [--background] [--inject] [--suppress-system-gestures] [--contacts 1..5] [--device 0] [--seconds 60] [--out NEW_DIRECTORY]\n  --self-test [--suppress-system-gestures] [--contacts 1..5] [--seconds 40] [--out NEW_DIRECTORY]\n  --verify-log PATH_TO_EVENTS_LOG\n--background starts raw-only capture minimized without requesting foreground.\nGesture suppression requires a supported foreground-process API; capture without --inject isolates physical gestures.\n--contacts 1: single-contact MVP; default 5: synchronized multitouch.\nSelf-test injects SYNTHETIC coordinates into a real Windows receiver; run manually.\nNo GUI or injection is started by cargo test or --verify-log. ESC releases all contacts and stops.");
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
        suppress_gestures: false,
        background: false,
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
            "--suppress-system-gestures" => opt.suppress_gestures = true,
            "--background" => opt.background = true,
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
    if opt.background && (opt.mode != "capture" || opt.inject) {
        return Err("--background is a raw-only --capture diagnostic; omit --inject".into());
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
            vec!["--self-test", "--background"],
            vec!["--capture", "--inject", "--background"],
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
    #[test]
    fn gesture_guard_is_explicit_and_can_be_exercised_without_hardware() {
        let o = parse_options(&args(&[
            "--capture",
            "--inject",
            "--suppress-system-gestures",
        ]))
        .unwrap();
        assert!(o.suppress_gestures);
        let raw = parse_options(&args(&["--capture", "--suppress-system-gestures"])).unwrap();
        assert!(raw.suppress_gestures);
        assert!(!raw.inject);
        let background = parse_options(&args(&["--capture", "--background"])).unwrap();
        assert!(background.background);
        let synthetic =
            parse_options(&args(&["--self-test", "--suppress-system-gestures"])).unwrap();
        assert!(synthetic.suppress_gestures);
        let o = parse_options(&args(&["--capture", "--inject"])).unwrap();
        assert!(!o.suppress_gestures);
    }
}
