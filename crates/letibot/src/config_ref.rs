//! `letibot --config-reference [FILE]` and `letibot --config-header FILE`: the options
//! reference from `letibot-config-ref`, printed or written into a TOML file's header block.

use std::path::Path;
use std::process::ExitCode;

use letibot_config_ref as cref;

const REFERENCE_USAGE: &str = "\
letibot --config-reference [--markdown] [FILE]
  Every option letibot reads from its config files: key, type, default, level, meaning.
  No FILE lists the files; FILE is a name such as `leticode.toml` or `providers.toml`.
  --markdown prints docs/config/<FILE>.md instead.";

const HEADER_USAGE: &str = "\
letibot --config-header FILE [--as NAME]
  Replace or insert the reference block at the top of a TOML config file. Writes FILE.bak
  first and changes nothing outside the block's markers. The reference is chosen by FILE's
  name; --as NAME picks it for a file named otherwise.";

pub fn reference(args: &[String]) -> ExitCode {
    let mut markdown = false;
    let mut name = None;
    for a in args {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{REFERENCE_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--markdown" => markdown = true,
            other if name.is_none() => name = Some(other.to_string()),
            other => {
                return refuse(
                    &format!("one FILE at most; `{other}` is a second"),
                    REFERENCE_USAGE,
                );
            }
        }
    }
    let Some(name) = name else {
        print!("{}", cref::overview());
        return ExitCode::SUCCESS;
    };
    let Some(f) = lookup(&name) else {
        return unknown(&name);
    };
    if markdown {
        print!("{}", cref::markdown(f));
    } else {
        print!("{}", cref::text(f));
    }
    ExitCode::SUCCESS
}

pub fn header(args: &[String]) -> ExitCode {
    let (mut file, mut as_name) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{HEADER_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--as" => match it.next() {
                Some(n) => as_name = Some(n.clone()),
                None => return refuse("--as takes a file name", HEADER_USAGE),
            },
            other if file.is_none() => file = Some(other.to_string()),
            other => return refuse(&format!("one FILE; `{other}` is a second"), HEADER_USAGE),
        }
    }
    let Some(file) = file else {
        return refuse("FILE is required", HEADER_USAGE);
    };
    let path = Path::new(&file);
    let name = as_name.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let Some(f) = lookup(&name) else {
        return unknown(&name);
    };
    match write_header(path, f) {
        Ok(true) => {
            println!(
                "{}: reference block written ({} kept)",
                path.display(),
                bak(path)
            );
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("{}: reference block already current", path.display());
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("letibot: {why}");
            ExitCode::FAILURE
        }
    }
}

fn bak(path: &Path) -> String {
    format!("{}.bak", path.display())
}

/// Splice the block into `path`, keeping the old bytes in `path.bak`. `false` when the file
/// already carries the current block, which writes nothing.
pub fn write_header(path: &Path, f: &cref::File) -> Result<bool, String> {
    if f.format != cref::Format::Toml {
        return Err(format!(
            "{} is {:?} and cannot hold comments; see `letibot --config-reference {}`",
            f.name, f.format, f.name
        ));
    }
    let old = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let new = cref::splice(&old, f).map_err(|why| format!("{}: {why}", path.display()))?;
    if new == old {
        return Ok(false);
    }
    std::fs::write(bak(path), &old).map_err(|e| format!("{}: {e}", bak(path)))?;
    let tmp = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, &new).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

fn lookup(name: &str) -> Option<&'static cref::File> {
    cref::find(name)
}

fn unknown(name: &str) -> ExitCode {
    let names: Vec<&str> = cref::ALL.iter().map(|f| f.name).collect();
    refuse(
        &format!(
            "no reference for `{name}`; the files are {}",
            names.join(", ")
        ),
        "",
    )
}

fn refuse(why: &str, usage: &str) -> ExitCode {
    eprintln!("letibot: {why}");
    if !usage.is_empty() {
        eprintln!("{usage}");
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ODD: &str = "# hand-written, keep me\n\
[default]\n\
provider   =   \"glm\"   # trailing comment\n\
\n\
\t[model.\"local.glm\"]\n\
temperature=0.6\n\
# no newline at the end";

    /// The header round trip on a hand-edited file: `.bak` holds the old bytes, the new file is
    /// the block plus the old bytes, a second run writes nothing, and the TOML still parses.
    #[test]
    fn config_header_round_trips_a_hand_edited_file() {
        let dir =
            std::env::temp_dir().join(format!("letibot-config-header-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.toml");
        std::fs::write(&path, ODD).unwrap();
        let f = cref::find("providers.toml").unwrap();

        assert!(write_header(&path, f).unwrap());
        let new = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("providers.toml.bak")).unwrap(),
            ODD
        );
        assert_eq!(new, format!("{}{ODD}", cref::header(f)));
        assert_eq!(cref::strip(&new), ODD);
        let _: toml::Table = toml::from_str(&new).unwrap();

        // A stale block is replaced, and nothing around it moves.
        let stale = new.replace("#   provider:", "#   provider (old):");
        std::fs::write(&path, &stale).unwrap();
        assert!(write_header(&path, f).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), new);
        assert!(
            !write_header(&path, f).unwrap(),
            "a current block was rewritten"
        );

        let json = dir.join("permission.json");
        std::fs::write(&json, "{}").unwrap();
        assert!(write_header(&json, cref::find("permission.json").unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
