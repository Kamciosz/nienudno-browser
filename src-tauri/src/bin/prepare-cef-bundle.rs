use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn find_cef_dir(target_release: &Path) -> io::Result<PathBuf> {
    let build_dir = target_release.join("build");
    let prefix = format!("cef_{}", env::consts::OS);
    let mut matches = fs::read_dir(build_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("out"))
        .filter(|path| path.is_dir())
        .flat_map(|out_dir| {
            fs::read_dir(out_dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
        })
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
        });
    matches.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Nie znaleziono pobranych plików CEF.",
        )
    })
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

const MACOS_HELPER_SUFFIXES: &[&str] = &["", " (GPU)", " (Renderer)", " (Plugin)", " (Alerts)"];

fn helper_info_plist(helper_name: &str) -> String {
    let identifier_suffix = helper_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/cef-helper/Info.plist"
    ))
    .replace("__HELPER_NAME__", helper_name)
    .replace(
        "__HELPER_IDENTIFIER__",
        &format!("pl.nienudno.browser.helper.{identifier_suffix}"),
    )
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn prepare_macos(target_release: &Path, cef_dir: &Path, stage: &Path) -> io::Result<()> {
    let framework = cef_dir.join("Chromium Embedded Framework.framework");
    if !framework.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Nie znaleziono frameworka CEF: {}", framework.display()),
        ));
    }
    let helper_binary = target_release.join("nienudno-browser-helper");
    if !helper_binary.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Nie znaleziono helpera CEF: {}", helper_binary.display()),
        ));
    }

    copy_tree(
        &framework,
        &stage.join("Chromium Embedded Framework.framework"),
    )?;
    for suffix in MACOS_HELPER_SUFFIXES {
        let helper_name = format!("NieNudno Browser Helper{suffix}");
        let helper_app = stage.join(format!("{helper_name}.app"));
        let helper_contents = helper_app.join("Contents");
        let helper_executable = helper_contents.join("MacOS").join(&helper_name);
        fs::create_dir_all(helper_contents.join("MacOS"))?;
        fs::copy(&helper_binary, &helper_executable)?;
        fs::write(
            helper_contents.join("Info.plist"),
            helper_info_plist(&helper_name),
        )?;
        make_executable(&helper_executable)?;
    }
    Ok(())
}

fn prepare_other(target_release: &Path, cef_dir: &Path, stage: &Path) -> io::Result<()> {
    let _ = target_release;
    copy_tree(cef_dir, stage)
}

fn main() -> io::Result<()> {
    let executable = env::current_exe()?;
    let target_release = executable
        .parent()
        .ok_or_else(|| io::Error::other("Nieprawidłowa ścieżka programu przygotowującego."))?;
    let cef_dir = find_cef_dir(target_release)?;
    let stage = target_release.join("cef-bundle");
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;

    if cfg!(target_os = "macos") {
        prepare_macos(target_release, &cef_dir, &stage)?;
    } else {
        prepare_other(target_release, &cef_dir, &stage)?;
    }
    println!("Przygotowano pliki CEF w {}", stage.display());
    Ok(())
}
