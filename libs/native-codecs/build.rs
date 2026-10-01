use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Toolchain {
    cc: PathBuf,
    cxx: PathBuf,
    cflags: String,
    cxxflags: String,
    ar: String,
    ranlib: String,
    cross: bool,
    arch: String,
}

fn main() {
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        return;
    }
    if env::var("VCPKG_ROOT").is_ok() || env::var("CARGO_FEATURE_SYSTEM").is_ok() {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let tarballs = manifest.join("../third_party/tarballs");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let prefix = out_dir.join("prefix");
    let fingerprint = fingerprint(&tarballs);
    let stamp = out_dir.join("native-codecs.stamp");
    let tc = toolchain();
    // A Yocto or distro package is used when the target compiler can see it.
    // The bundled sources are compiled only for a library that is absent.
    let system = find_system_codecs(&out_dir);
    let mut fingerprint = fingerprint;
    for codec in &system {
        fingerprint.push('\n');
        fingerprint.push_str("system-");
        fingerprint.push_str(codec.name);
        fingerprint.push('=');
        fingerprint.push_str(codec.dir.as_deref().unwrap_or("-"));
    }
    fingerprint.push('\n');

    for codec in &system {
        if codec.dir.is_some() {
            drop_vendored(&prefix, codec.name);
        }
    }

    if stamp.exists() && fs::read_to_string(&stamp).ok().as_deref() == Some(fingerprint.as_str()) {
        emit_links(&prefix, &system);
        return;
    }

    fs::create_dir_all(&prefix).unwrap_or_else(|e| panic!("create {prefix:?}: {e}"));
    for codec in &system {
        if codec.dir.is_some() || archive_present(&prefix, codec.name) {
            continue;
        }
        match codec.name {
            "vpx" => build_libvpx(&tc, &tarballs, &out_dir, &prefix),
            "aom" => build_aom(&tc, &tarballs, &out_dir, &prefix),
            "opus" => build_opus(&tc, &tarballs, &out_dir, &prefix),
            "yuv" => build_libyuv(&tc, &tarballs, &out_dir, &prefix),
            _ => {}
        }
    }

    fs::write(&stamp, fingerprint).unwrap_or_else(|e| panic!("write stamp: {e}"));
    emit_links(&prefix, &system);
}

fn fingerprint(tarballs: &Path) -> String {
    let mut parts = Vec::new();
    for name in ["libvpx-1.15.2.tar.gz", "libaom-3.14.1.tar.gz", "opus-1.5.2.tar.gz", "libyuv.tar.gz"]
    {
        let path = tarballs.join(name);
        let meta = fs::metadata(&path).unwrap_or_else(|_| {
            panic!(
                "missing codec source {path:?}. It ships in the repo; do not delete libs/third_party/tarballs."
            )
        });
        println!("cargo:rerun-if-changed={}", path.display());
        parts.push(format!("{name}:{}", meta.len()));
    }
    parts.join("\n")
}

struct SystemCodec {
    name: &'static str,
    dir: Option<String>,
    encoder_abi: Option<u32>,
    decoder_abi: Option<u32>,
}

fn find_system_codecs(out_dir: &Path) -> Vec<SystemCodec> {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_SYSROOT_DIR");
    vec![
        system_codec(
            out_dir,
            "vpx",
            &["vpx", "libvpx"],
            "#include <vpx/vpx_encoder.h>\nint main() { return 0; }\n",
            "libvpx.so",
            false,
            Some(AbiHeaders {
                encoder_header: "vpx/vpx_encoder.h",
                encoder_macro: "VPX_ENCODER_ABI_VERSION",
                decoder_header: "vpx/vpx_decoder.h",
                decoder_macro: "VPX_DECODER_ABI_VERSION",
            }),
        ),
        system_codec(
            out_dir,
            "aom",
            &["aom", "libaom"],
            "#include <aom/aom.h>\nint main() { return 0; }\n",
            "libaom.so",
            false,
            Some(AbiHeaders {
                encoder_header: "aom/aom_encoder.h",
                encoder_macro: "AOM_ENCODER_ABI_VERSION",
                decoder_header: "aom/aom_decoder.h",
                decoder_macro: "AOM_DECODER_ABI_VERSION",
            }),
        ),
        system_codec(
            out_dir,
            "opus",
            &["opus", "libopus"],
            "#include <opus/opus.h>\nint main() { return 0; }\n",
            "libopus.so",
            false,
            None,
        ),
        system_codec(
            out_dir,
            "yuv",
            &["libyuv", "yuv"],
            "#include <libyuv.h>\nint main() { return 0; }\n",
            "libyuv.so",
            true,
            None,
        ),
    ]
}

fn drop_vendored(prefix: &Path, name: &str) {
    let stem = format!("lib{name}.");
    for sub in ["lib", "lib64"] {
        let Ok(entries) = fs::read_dir(prefix.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with(&stem) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn archive_present(prefix: &Path, name: &str) -> bool {
    ["lib", "lib64"].iter().any(|sub| {
        prefix
            .join(sub)
            .join(format!("lib{name}.a"))
            .is_file()
    })
}

fn emit_links(prefix: &Path, system: &[SystemCodec]) {
    let include = prefix.join("include");
    let _ = fs::create_dir_all(&include);
    for codec in system {
        if codec.dir.is_some() {
            continue;
        }
        let header = match codec.name {
            "vpx" => "vpx",
            "aom" => "aom",
            "opus" => "opus",
            "yuv" => "libyuv.h",
            _ => continue,
        };
        if !include.join(header).exists() {
            panic!(
                "{} headers missing under {}. No system library was found, and the bundled build did not install.",
                codec.name,
                include.display()
            );
        }
    }
    for sub in ["lib", "lib64"] {
        let dir = prefix.join(sub);
        if dir.is_dir() {
            println!("cargo:libdir={}", dir.display());
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
    }
    for codec in system {
        if let Some(dir) = codec.dir.as_deref() {
            println!("cargo:system-{}=1", codec.name);
            println!("cargo:{}-libdir={dir}", codec.name);
        }
        if let Some(v) = codec.encoder_abi {
            println!("cargo:{}-encoder-abi={v}", codec.name);
        }
        if let Some(v) = codec.decoder_abi {
            println!("cargo:{}-decoder-abi={v}", codec.name);
        }
    }
    println!("cargo:include={}", include.display());
    println!("cargo:vendored=1");
}

struct AbiHeaders {
    encoder_header: &'static str,
    encoder_macro: &'static str,
    decoder_header: &'static str,
    decoder_macro: &'static str,
}

fn system_codec(
    out_dir: &Path,
    name: &'static str,
    pc_names: &[&str],
    source: &str,
    so_name: &str,
    cpp: bool,
    abi: Option<AbiHeaders>,
) -> SystemCodec {
    let dir = find_system_lib(out_dir, name, pc_names, source, so_name, cpp);
    let (encoder_abi, decoder_abi) = match (dir.as_ref(), abi) {
        (Some(_), Some(abi)) => {
            let encoder = header_int(abi.encoder_header, abi.encoder_macro, cpp);
            let decoder = header_int(abi.decoder_header, abi.decoder_macro, cpp);
            if encoder.is_none() || decoder.is_none() {
                println!(
                    "cargo:warning=system lib{name} is installed, but its encoder ABI version could not be read, so the bundled library is built"
                );
                return SystemCodec {
                    name,
                    dir: None,
                    encoder_abi: None,
                    decoder_abi: None,
                };
            }
            (encoder, decoder)
        }
        _ => (None, None),
    };
    if dir.is_some() {
        println!("cargo:warning=using system lib{name}");
    }
    SystemCodec {
        name,
        dir,
        encoder_abi,
        decoder_abi,
    }
}

/// Integer value of a codec ABI macro from the headers the target compiler sees.
fn header_int(header: &str, macro_name: &str, cpp: bool) -> Option<u32> {
    let mut cmd = cc::Build::new().cpp(cpp).get_compiler().to_command();
    cmd.arg("-E")
        .arg("-P")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    {
        let stdin = child.stdin.as_mut()?;
        let src = format!("#include <{header}>\n{macro_name}\n");
        std::io::Write::write_all(stdin, src.as_bytes()).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let expr = text.lines().last()?.trim();
    eval_int_expr(expr)
}

fn eval_int_expr(expr: &str) -> Option<u32> {
    fn eval(tokens: &mut &str) -> Option<i64> {
        let mut acc = eval_term(tokens)?;
        loop {
            let s = tokens.trim_start();
            if let Some(rest) = s.strip_prefix('+') {
                *tokens = rest;
                acc += eval_term(tokens)?;
            } else {
                *tokens = s;
                break;
            }
        }
        Some(acc)
    }
    fn eval_term(tokens: &mut &str) -> Option<i64> {
        let s = tokens.trim_start();
        if let Some(rest) = s.strip_prefix('(') {
            *tokens = rest;
            let v = eval(tokens)?;
            let s = tokens.trim_start();
            *tokens = s.strip_prefix(')')?;
            return Some(v);
        }
        let s = tokens.trim_start();
        let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        *tokens = &s[digits.len()..];
        digits.parse().ok()
    }
    let mut tokens = expr;
    let value = eval(&mut tokens)?;
    if !tokens.trim().is_empty() {
        return None;
    }
    u32::try_from(value).ok()
}

/// `Some(dir)` when the target compiler can link this system library.
/// `dir` is the library directory, or empty when it is already on the default search path.
fn find_system_lib(
    out_dir: &Path,
    name: &str,
    pc_names: &[&str],
    source: &str,
    so_name: &str,
    cpp: bool,
) -> Option<String> {
    for pc in pc_names {
        if let Some(dir) = pkg_config_libdir(pc) {
            return Some(dir);
        }
    }
    let ext = if cpp { "cc" } else { "c" };
    let src = out_dir.join(format!("{name}_probe.{ext}"));
    let bin = out_dir.join(format!("{name}_probe.bin"));
    if fs::write(&src, source).is_err() {
        return None;
    }
    let mut cmd = cc::Build::new().cpp(cpp).get_compiler().to_command();
    cmd.arg(&src)
        .arg(format!("-l{name}"))
        .arg("-o")
        .arg(&bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let linked = cmd.status().ok().is_some_and(|s| s.success());
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&bin);
    if !linked {
        return None;
    }
    let mut which = cc::Build::new().cpp(cpp).get_compiler().to_command();
    which.arg(format!("-print-file-name={so_name}"));
    let printed = which
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let dir = if printed.is_empty() || printed == so_name {
        String::new()
    } else {
        Path::new(&printed)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    Some(dir)
}

fn pkg_config_libdir(name: &str) -> Option<String> {
    let exists = Command::new("pkg-config")
        .args(["--exists", name])
        .status()
        .ok()?;
    if !exists.success() {
        return None;
    }
    let output = Command::new("pkg-config")
        .args(["--libs-only-L", name])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for part in text.split_whitespace() {
        if let Some(dir) = part.strip_prefix("-L") {
            return Some(dir.to_owned());
        }
    }
    Some(String::new())
}

fn toolchain() -> Toolchain {
    let c = cc::Build::new().get_compiler();
    let cxx = cc::Build::new().cpp(true).get_compiler();
    let host = env::var("HOST").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();
    Toolchain {
        cc: c.path().to_path_buf(),
        cxx: cxx.path().to_path_buf(),
        cflags: with_pic(&join_args(c.args())),
        cxxflags: with_pic(&join_args(cxx.args())),
        ar: resolve_tool(&env::var("AR").unwrap_or_else(|_| "ar".to_owned())),
        ranlib: resolve_tool(&env::var("RANLIB").unwrap_or_else(|_| "ranlib".to_owned())),
        cross: host != target,
        arch: env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default(),
    }
}

/// CMake treats a relative `CMAKE_AR` as a path inside the build tree.
fn resolve_tool(name: &str) -> String {
    let path = Path::new(name);
    if path.is_absolute() {
        return name.to_owned();
    }
    let search = env::var("PATH").unwrap_or_default();
    for dir in search.split(':') {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return candidate.display().to_string();
        }
    }
    name.to_owned()
}

fn join_args(args: &[std::ffi::OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn with_pic(flags: &str) -> String {
    if flags.split_whitespace().any(|f| f == "-fPIC") {
        flags.to_owned()
    } else if flags.is_empty() {
        "-fPIC".to_owned()
    } else {
        format!("{flags} -fPIC")
    }
}

fn apply(cmd: &mut Command, tc: &Toolchain) {
    cmd.env("CC", &tc.cc);
    cmd.env("CXX", &tc.cxx);
    cmd.env("CFLAGS", &tc.cflags);
    cmd.env("CXXFLAGS", &tc.cxxflags);
    cmd.env("AR", &tc.ar);
    cmd.env("RANLIB", &tc.ranlib);
    // Cargo's jobserver fds are not valid in this child and make libvpx's make fail.
    cmd.env_remove("MAKEFLAGS");
    cmd.env_remove("MFLAGS");
}

fn run(what: &str, dir: &Path, tc: &Toolchain, mut cmd: Command) {
    apply(&mut cmd, tc);
    cmd.current_dir(dir);
    // Child stdout must not mix into this script's stdout. Cargo only
    // reliably applies `cargo:rustc-link-*` lines from a build script whose
    // stdout is the instruction stream.
    let log_dir = dir.join("native-codecs-logs");
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.log", what.replace(' ', "_")));
    let log = fs::File::create(&log_path).unwrap_or_else(|e| panic!("log {log_path:?}: {e}"));
    cmd.stdout(std::process::Stdio::from(log.try_clone().unwrap()));
    cmd.stderr(std::process::Stdio::from(log));
    println!("cargo:warning={what}");
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{what} failed to start: {e}"));
    if !status.success() {
        let tail = fs::read_to_string(&log_path).unwrap_or_default();
        let tail = tail
            .lines()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        panic!("{what} exited with {status}\n{tail}");
    }
}

fn jobs() -> String {
    std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "1".to_owned())
}

fn extract(tarball: &Path, dest: &Path) -> PathBuf {
    if dest.exists() {
        let _ = fs::remove_dir_all(dest);
    }
    fs::create_dir_all(dest).unwrap_or_else(|e| panic!("mkdir {dest:?}: {e}"));
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(tarball)
        .arg("-C")
        .arg(dest)
        .status()
        .unwrap_or_else(|e| panic!("tar {tarball:?}: {e}"));
    if !status.success() {
        panic!("tar {tarball:?} exited with {status}");
    }
    let mut dirs: Vec<_> = fs::read_dir(dest)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if dirs.len() != 1 {
        panic!("expected one directory inside {tarball:?}, found {dirs:?}");
    }
    dirs.pop().unwrap()
}

fn vpx_target(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "x86_64-linux-gcc",
        "x86" => "x86-linux-gcc",
        "aarch64" => "arm64-linux-gcc",
        "arm" => "armv7-linux-gcc",
        "loongarch64" => "loongarch64-linux-gcc",
        _ => "generic-gnu",
    }
}

fn aom_cpu(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "x86_64",
        "x86" => "x86",
        "aarch64" => "arm64",
        "arm" => "arm",
        _ => "generic",
    }
}

fn build_libvpx(tc: &Toolchain, tarballs: &Path, out: &Path, prefix: &Path) {
    let src = extract(&tarballs.join("libvpx-1.15.2.tar.gz"), &out.join("src-libvpx"));
    let build = out.join("build-libvpx");
    let _ = fs::remove_dir_all(&build);
    fs::create_dir_all(&build).unwrap();
    let configure = src.join("configure");
    let mut cmd = Command::new("sh");
    cmd.arg(&configure)
        .arg(format!("--prefix={}", prefix.display()))
        .arg(format!("--target={}", vpx_target(&tc.arch)))
        .arg("--enable-pic")
        .arg("--enable-static")
        .arg("--disable-shared")
        .arg("--disable-examples")
        .arg("--disable-tools")
        .arg("--disable-docs")
        .arg("--disable-unit-tests")
        .arg("--disable-install-bins")
        .arg(format!("--extra-cflags={}", tc.cflags))
        .arg(format!("--extra-cxxflags={}", tc.cxxflags));
    run("configure libvpx", &build, tc, cmd);
    let mut make = Command::new("make");
    make.arg(format!("-j{}", jobs()));
    run("build libvpx", &build, tc, make);
    let mut install = Command::new("make");
    install.arg("install");
    run("install libvpx", &build, tc, install);
}

fn cmake_common(tc: &Toolchain, prefix: &Path) -> Command {
    let mut cmd = Command::new("cmake");
    cmd.arg("-G")
        .arg("Unix Makefiles")
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", prefix.display()))
        .arg("-DCMAKE_INSTALL_LIBDIR=lib")
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DBUILD_SHARED_LIBS=OFF")
        .arg("-DCMAKE_POSITION_INDEPENDENT_CODE=ON")
        .arg(format!("-DCMAKE_C_COMPILER={}", tc.cc.display()))
        .arg(format!("-DCMAKE_CXX_COMPILER={}", tc.cxx.display()))
        .arg(format!("-DCMAKE_C_FLAGS={}", tc.cflags))
        .arg(format!("-DCMAKE_CXX_FLAGS={}", tc.cxxflags))
        .arg(format!("-DCMAKE_AR={}", tc.ar))
        .arg(format!("-DCMAKE_RANLIB={}", tc.ranlib));
    if tc.cross {
        cmd.arg("-DCMAKE_SYSTEM_NAME=Linux");
        cmd.arg(format!(
            "-DCMAKE_SYSTEM_PROCESSOR={}",
            match tc.arch.as_str() {
                "aarch64" => "aarch64",
                "arm" => "arm",
                "x86_64" => "x86_64",
                other => other,
            }
        ));
        // Do not try to run a test program built for the board.
        cmd.arg("-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY");
    }
    cmd
}

fn cmake_build_install(what: &str, tc: &Toolchain, build: &Path) {
    let mut build_cmd = Command::new("cmake");
    build_cmd
        .arg("--build")
        .arg(".")
        .arg("--parallel")
        .arg(jobs());
    run(&format!("build {what}"), build, tc, build_cmd);
    let mut install = Command::new("cmake");
    install.arg("--install").arg(".");
    run(&format!("install {what}"), build, tc, install);
}

fn build_aom(tc: &Toolchain, tarballs: &Path, out: &Path, prefix: &Path) {
    let src = extract(&tarballs.join("libaom-3.14.1.tar.gz"), &out.join("src-aom"));
    let build = out.join("build-aom");
    let _ = fs::remove_dir_all(&build);
    fs::create_dir_all(&build).unwrap();
    let mut cmd = cmake_common(tc, prefix);
    let nasm = if tc.arch == "x86_64" || tc.arch == "x86" {
        "ON"
    } else {
        "OFF"
    };
    cmd.arg(&src)
        .arg("-DENABLE_TESTS=OFF")
        .arg("-DENABLE_EXAMPLES=OFF")
        .arg("-DENABLE_TOOLS=OFF")
        .arg("-DENABLE_DOCS=OFF")
        .arg("-DENABLE_TESTDATA=OFF")
        .arg("-DCONFIG_AV1_ENCODER=1")
        .arg("-DCONFIG_AV1_DECODER=1")
        .arg("-DCONFIG_MULTITHREAD=1")
        .arg("-DCONFIG_PIC=1")
        .arg(format!("-DENABLE_NASM={nasm}"))
        .arg(format!("-DAOM_TARGET_CPU={}", aom_cpu(&tc.arch)));
    run("configure libaom", &build, tc, cmd);
    cmake_build_install("libaom", tc, &build);
}

fn opus_host(tc: &Toolchain) -> Option<String> {
    if !tc.cross {
        return None;
    }
    let name = tc.cc.file_name()?.to_str()?;
    for suffix in ["-gcc", "-cc"] {
        if let Some(triplet) = name.strip_suffix(suffix) {
            if !triplet.is_empty() {
                return Some(triplet.to_owned());
            }
        }
    }
    None
}

fn build_opus(tc: &Toolchain, tarballs: &Path, out: &Path, prefix: &Path) {
    let src = extract(&tarballs.join("opus-1.5.2.tar.gz"), &out.join("src-opus"));
    let build = out.join("build-opus");
    let _ = fs::remove_dir_all(&build);
    fs::create_dir_all(&build).unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg(src.join("configure"))
        .arg(format!("--prefix={}", prefix.display()))
        .arg("--disable-shared")
        .arg("--enable-static")
        .arg("--disable-doc")
        .arg("--disable-extra-programs")
        .arg("--with-pic");
    if let Some(host) = opus_host(tc) {
        cmd.arg(format!("--host={host}"));
    }
    run("configure opus", &build, tc, cmd);
    let mut make = Command::new("make");
    make.arg(format!("-j{}", jobs()));
    run("build opus", &build, tc, make);
    let mut install = Command::new("make");
    install.arg("install");
    run("install opus", &build, tc, install);
}

fn build_libyuv(tc: &Toolchain, tarballs: &Path, out: &Path, prefix: &Path) {
    let src = extract(&tarballs.join("libyuv.tar.gz"), &out.join("src-libyuv"));
    let build = out.join("build-libyuv");
    let _ = fs::remove_dir_all(&build);
    fs::create_dir_all(&build).unwrap();
    let mut cmd = cmake_common(tc, prefix);
    cmd.arg(&src)
        .arg("-DLIBYUV_DISABLE_JPEG=ON")
        .arg("-DUNIT_TEST=OFF");
    run("configure libyuv", &build, tc, cmd);
    cmake_build_install("libyuv", tc, &build);
}
