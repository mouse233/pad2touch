mod decode;
mod experiment;
mod hid;
mod injection;
mod logging;
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] {
        println!("PadBridge Phase 0\n  --probe\n  --capture [--inject] [--device 0] [--seconds 60] [--out artifacts/run]\n  --self-test [--seconds 12] [--out artifacts/self-test]\nCapture records physical Raw Input. --inject maps ONE contact to a fixed test rectangle.\nSelf-test uses synthetic coordinates to test Windows injection ONLY. ESC stops the test.");
        return Ok(());
    }
    if args == ["--probe"] {
        return hid::probe();
    }
    let mut opt = experiment::Options {
        mode: String::new(),
        inject: false,
        device: 0,
        seconds: 60,
        out: "artifacts/run".into(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--capture" => opt.mode = "capture".into(),
            "--self-test" => opt.mode = "self-test".into(),
            "--inject" => opt.inject = true,
            "--device" | "--seconds" | "--out" => {
                let key = &args[i];
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| format!("{key} requires a value"))?;
                match key.as_str() {
                    "--device" => opt.device = v.parse().map_err(|_| "invalid device index")?,
                    "--seconds" => opt.seconds = v.parse().map_err(|_| "invalid seconds")?,
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
    experiment::run(opt)
}
