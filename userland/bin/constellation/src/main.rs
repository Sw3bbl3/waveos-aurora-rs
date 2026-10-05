#![no_std]
#![no_main]
extern crate alloc;
use alloc::{
    format,
    string::{String, ToString},
};
use constellation_sdk as sdk;
use corekit::{apps, fs, println};
corekit::entry!(main);
fn main(args: corekit::Args) -> i32 {
    match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            corekit::eprintln!("constellation: {e}");
            1
        }
    }
}
fn run(args: &[String]) -> sdk::Result<()> {
    let required = |i: usize| {
        args.get(i).map(String::as_str).ok_or_else(|| String::from("Missing argument. Run constellation help."))
    };
    match args.get(1).map(String::as_str).unwrap_or("help") {
        "new"=>{sdk::new_project(required(2)?,required(3)?,args.get(4).map(String::as_str).unwrap_or("My App"))?;println!("Created {}",required(2)?);}
        "doctor"=>{let missing=sdk::doctor();if missing.is_empty(){println!("All native tool executables are present.");}else{for line in &missing{println!("{line}");}return Err("Native compilation is not available in this image.".into());}}
        "build"=>{sdk::build(required(2)?,|s|corekit::print!("{s}"),&core::sync::atomic::AtomicBool::new(false))?;println!("Build succeeded.");}
        "package"=>{let(path,bytes)=sdk::package(required(2)?)?;println!("Exported {} ({} bytes)",path,bytes.len());}
        "install"=>{let bytes=fs::read(required(2)?).map_err(|e|format!("{e}"))?;let app=apps::install(&bytes,|_,_|{})?;println!("Installed {} {}",app.manifest.name,app.manifest.version);}
        "inspect"=>{let bytes=fs::read(required(2)?).map_err(|e|format!("{e}"))?;let p=gina::Package::parse(&bytes).map_err(ToString::to_string)?;println!("{}\n{}\n{} · {}\n{} files",p.manifest.name,p.manifest.id,p.manifest.developer,p.manifest.version,p.files.len());}
        "list"=>for app in apps::list(){println!("{}  {}  {}",app.manifest.id,app.manifest.version,app.manifest.name);},
        "run"=>{let project=required(2)?;sdk::build(project,|s|corekit::print!("{s}"),&core::sync::atomic::AtomicBool::new(false))?;let(_,bytes)=sdk::package(project)?;let app=apps::install(&bytes,|_,_|{})?;let pid=apps::launch(&app.manifest.id)?;println!("Running {} (pid {})",app.manifest.name,pid);}
        "launch"=>{println!("Started pid {}",apps::launch(required(2)?)?);}
        "uninstall"=>{apps::uninstall(required(2)?,args.iter().any(|a|a=="--delete-data"))?;println!("Uninstalled.");}
        "generate"=>sdk::generate_ui(required(2)?)?,
        _=>println!("Constellation SDK\n  new /Documents/Projects/App dev.example.app \"My App\"\n  doctor\n  build PROJECT\n  generate PROJECT\n  run PROJECT\n  package PROJECT\n  inspect PACKAGE.gina\n  install PACKAGE.gina\n  list\n  launch APP_ID\n  uninstall APP_ID [--delete-data]"),
    }
    Ok(())
}
