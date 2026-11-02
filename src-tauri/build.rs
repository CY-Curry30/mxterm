use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=rdp-host");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build_rdp_host();
    }
    tauri_build::build()
}

fn build_rdp_host() {
    let windows = PathBuf::from(env::var_os("WINDIR").expect("Windows directory"));
    let compiler = windows.join("Microsoft.NET/Framework64/v4.0.30319/csc.exe");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("mxterm-rdp-host.exe");
    let target = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let platform = match target.as_str() {
        "x86" => "/platform:x86",
        "x86_64" => "/platform:x64",
        // CLR JIT uses the native architecture on Windows ARM64.
        "aarch64" => "/platform:anycpu",
        _ => panic!("Unsupported Windows RDP host architecture: {target}"),
    };
    let status = Command::new(compiler)
        .args([
            "/nologo",
            "/target:winexe",
            platform,
            "/optimize+",
            "/utf8output",
            "/warnaserror+",
            "/nowarn:0649",
        ])
        .arg(format!("/out:{}", output.display()))
        .args([
            "/win32manifest:rdp-host\\app.manifest",
            "/win32icon:icons/icon.ico",
            "/reference:System.Windows.Forms.dll",
            "/reference:System.Drawing.dll",
            "/reference:System.Web.Extensions.dll",
        ])
        .args([
            "rdp-host\\Program.cs",
            "rdp-host\\Protocol.cs",
            "rdp-host\\Interop.cs",
            "rdp-host\\HostChrome.cs",
            "rdp-host\\HostWindow.cs",
            "rdp-host\\SelfTest.cs",
        ])
        .status()
        .expect("The Windows build requires the installed .NET Framework 4 C# compiler");
    assert!(
        status.success(),
        "Could not compile the Windows RDP AxHost helper"
    );
}
