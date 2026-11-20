fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        stage_pdfium();
        embed_windows_icon();
    }
    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new().with_edition((1, 19)),
    )
    .expect("compile TinyPDF UI");
}

#[cfg(windows)]
fn stage_pdfium() {
    use std::{env, fs, path::PathBuf, process::Command};

    let platform = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "win-x64",
        Ok("aarch64") => "win-arm64",
        _ => panic!("TinyPDF supports Windows x64 and ARM64."),
    };
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let script = root.join("scripts/prepare-pdfium.ps1");
    let supplied = env::var_os("TINYPDF_PDFIUM_ARCHIVE").map(PathBuf::from);
    let archive = supplied
        .as_ref()
        .map(|path| root.join(path))
        .unwrap_or_else(|| root.join(format!("target/pdfium-download/pdfium-{platform}.tgz")));
    // Cargo's OUT_DIR is under <profile>/build/<package>/out.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).expect("Cargo profile directory");
    let runtime = profile.join("pdfium.dll");
    let test_runtime = profile.join("deps/pdfium.dll");
    println!("cargo:rerun-if-env-changed=TINYPDF_PDFIUM_ARCHIVE");
    for path in [&script, &archive, &runtime, &test_runtime] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut prepare = Command::new("pwsh");
    prepare
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(script)
        .args(["-Platform", platform]);
    if supplied.is_some() {
        prepare.arg("-Archive").arg(archive);
    }
    let output = prepare
        .output()
        .expect("PowerShell 7 (pwsh) is required to prepare the Windows PDFium runtime");
    assert!(
        output.status.success(),
        "Could not prepare PDFium: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let bytes = fs::read(&source).expect("read prepared PDFium runtime");
    for path in [runtime, test_runtime] {
        if fs::read(&path)
            .as_deref()
            .is_ok_and(|current| current == bytes)
        {
            continue;
        }
        fs::create_dir_all(path.parent().unwrap()).expect("create Cargo artifact directory");
        fs::write(&path, &bytes)
            .unwrap_or_else(|error| panic!("Could not stage {}: {error}", path.display()));
    }
}

#[cfg(windows)]
fn embed_windows_icon() {
    use std::{env, path::PathBuf};

    // The shell reads the PE icon before the application window exists.
    let target = env::var("TARGET").expect("Cargo target");
    let rc =
        cc::windows_registry::find_tool(&target, "rc.exe").expect("Windows SDK resource compiler");
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("tinypdf.res");
    println!("cargo:rerun-if-changed=assets/tinypdf.ico");
    println!("cargo:rerun-if-changed=assets/tinypdf.rc");
    let status = rc
        .to_command()
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg("assets/tinypdf.rc")
        .status()
        .expect("run Windows SDK resource compiler");
    assert!(
        status.success(),
        "Could not compile the TinyPDF icon resource"
    );
    println!("cargo:rustc-link-arg-bin=tinypdf={}", resource.display());
}
