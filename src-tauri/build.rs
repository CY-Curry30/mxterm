use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=rdp-host");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build_rdp_host();
    }
    tauri_build::build()
}

// Task Manager's description column reads the Win32 version resource that the
// C# compiler derives from assembly attributes. Generate them from the crate
// version so the helper never reports a version the app does not have.
fn numeric_version(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts: Vec<&str> = core.split('.').take(4).collect();
    while parts.len() < 4 {
        parts.push("0");
    }
    parts.join(".")
}

fn write_assembly_info(version: &str) -> PathBuf {
    let path = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("RdpHostAssemblyInfo.cs");
    let numeric = numeric_version(version);
    let source = format!(
        "using System.Reflection;\r\n\r\n\
         [assembly: AssemblyTitle(\"MXterm RDP\")]\r\n\
         [assembly: AssemblyDescription(\"MXterm 远程桌面宿主\")]\r\n\
         [assembly: AssemblyProduct(\"MXterm\")]\r\n\
         [assembly: AssemblyCompany(\"MXterm\")]\r\n\
         [assembly: AssemblyCopyright(\"MIT License\")]\r\n\
         [assembly: AssemblyVersion(\"{numeric}\")]\r\n\
         [assembly: AssemblyFileVersion(\"{numeric}\")]\r\n\
         [assembly: AssemblyInformationalVersion(\"{version}\")]\r\n"
    );
    // The C# sources carry no BOM; emit one here so the Chinese description is
    // read as UTF-8 regardless of the machine's ANSI code page.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(source.as_bytes());
    fs::write(&path, bytes).expect("Could not write the RDP host assembly info");
    path
}

fn build_rdp_host() {
    let windows = PathBuf::from(env::var_os("WINDIR").expect("Windows directory"));
    let compiler = windows.join("Microsoft.NET/Framework64/v4.0.30319/csc.exe");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("mxterm-rdp.exe");
    let target = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let platform = match target.as_str() {
        "x86" => "/platform:x86",
        "x86_64" => "/platform:x64",
        // CLR JIT uses the native architecture on Windows ARM64.
        "aarch64" => "/platform:anycpu",
        _ => panic!("Unsupported Windows RDP host architecture: {target}"),
    };
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let assembly_info = write_assembly_info(&version);
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
        .arg(assembly_info)
        .status()
        .expect("The Windows build requires the installed .NET Framework 4 C# compiler");
    assert!(
        status.success(),
        "Could not compile the Windows RDP AxHost helper"
    );
}
