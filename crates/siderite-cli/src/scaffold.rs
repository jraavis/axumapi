//! `siderite new`: write a small API crate.

use crate::error::CliError;
use std::fs;
use std::path::{Path, PathBuf};

const GIT_DEP: &str = r#"git = "https://github.com/jraavis/siderite""#;

/// Database engine for a new project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Sqlite,
    Postgres,
    Mysql,
}

impl Engine {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            other => Err(CliError::usage(format!(
                "unknown --database `{other}` (sqlite, postgres or mysql)"
            ))),
        }
    }

    fn url(self, crate_name: &str) -> String {
        match self {
            Self::Sqlite => format!("sqlite://{crate_name}.db?mode=rwc"),
            Self::Postgres => {
                format!("postgres://siderite:siderite@127.0.0.1:5432/{crate_name}")
            }
            Self::Mysql => format!("mysql://siderite:siderite@127.0.0.1:3306/{crate_name}"),
        }
    }

    fn cli_features(self) -> &'static str {
        match self {
            Self::Sqlite => "",
            Self::Postgres => r#", features = ["postgres"]"#,
            Self::Mysql => r#", features = ["mysql"]"#,
        }
    }

    fn backends_dep(self, loc: &str) -> String {
        match self {
            Self::Sqlite => String::new(),
            Self::Postgres => {
                format!("siderite-backends = {{ {loc}, features = [\"postgres\"] }}\n")
            }
            Self::Mysql => {
                format!("siderite-backends = {{ {loc}, features = [\"mysql\"] }}\n")
            }
        }
    }
}

/// `siderite new NAME [--database ENGINE] [--path DIR]`.
pub fn run(args: &[String]) -> Result<u8, CliError> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(0);
    }
    let (name, engine, path) = parse(args)?;
    let dest = std::env::current_dir()
        .map_err(|err| io_err("read cwd", err))?
        .join(&name);
    write_project(&dest, &name, engine, path.as_deref())?;
    println!("Created {name}. Next:");
    println!("  cd {name}");
    println!("  siderite run");
    Ok(0)
}

fn parse(args: &[String]) -> Result<(String, Engine, Option<PathBuf>), CliError> {
    let mut name = None;
    let mut engine = Engine::Sqlite;
    let mut path = None;
    let mut i = 0;
    // `args` starts at `new`.
    if args.first().map(String::as_str) == Some("new") {
        i = 1;
    }
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--database" || arg.starts_with("--database=") {
            let value = if let Some(v) = arg.strip_prefix("--database=") {
                v.to_owned()
            } else {
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::usage("--database requires a value"))?;
                i += 1;
                v.clone()
            };
            engine = Engine::parse(&value)?;
            continue;
        }
        if arg == "--path" || arg.starts_with("--path=") {
            let value = if let Some(v) = arg.strip_prefix("--path=") {
                v.to_owned()
            } else {
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::usage("--path requires a value"))?;
                i += 1;
                v.clone()
            };
            path = Some(PathBuf::from(value));
            continue;
        }
        if arg.starts_with('-') {
            return Err(CliError::usage(format!("unknown flag `{arg}` for `new`")));
        }
        if name.is_some() {
            return Err(CliError::usage("unexpected extra argument for `new`"));
        }
        name = Some(arg.clone());
    }
    let name = name.ok_or_else(|| CliError::usage("usage: siderite new NAME"))?;
    validate_name(&name)?;
    Ok((name, engine, path))
}

fn validate_name(name: &str) -> Result<(), CliError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(CliError::usage("project name is empty"));
    };
    if !first.is_ascii_lowercase() {
        return Err(CliError::usage(
            "project name must start with a lowercase letter",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(CliError::usage(
            "project name may contain only lowercase letters, digits, `-` and `_`",
        ));
    }
    Ok(())
}

fn write_project(
    dest: &Path,
    name: &str,
    engine: Engine,
    siderite_root: Option<&Path>,
) -> Result<(), CliError> {
    if dest.exists() {
        let empty = dest
            .read_dir()
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(CliError::Io(format!(
                "`{}` already exists and is not empty",
                dest.display()
            )));
        }
    }
    fs::create_dir_all(dest.join("src")).map_err(|err| io_err("create src", err))?;
    fs::create_dir_all(dest.join("migrations")).map_err(|err| io_err("create migrations", err))?;

    let (siderite_loc, cli_loc, backends) = if let Some(root) = siderite_root {
        let root = root
            .canonicalize()
            .map_err(|err| io_err("resolve --path", err))?;
        let crate_path = |pkg: &str| {
            format!("path = \"{}\"", root.join("crates").join(pkg).display()).replace('\\', "/")
        };
        (
            crate_path("siderite"),
            format!("{}{}", crate_path("siderite-cli"), engine.cli_features()),
            engine.backends_dep(&crate_path("siderite-backends")),
        )
    } else {
        (
            GIT_DEP.to_owned(),
            format!("{GIT_DEP}{}", engine.cli_features()),
            engine.backends_dep(GIT_DEP),
        )
    };

    let ident = name.replace('-', "_");
    let url = engine.url(name);
    fs::write(
        dest.join("Cargo.toml"),
        format!(
            "\
[package]
name = \"{name}\"
version = \"0.1.0\"
edition = \"2024\"

[dependencies]
siderite = {{ {siderite_loc} }}
siderite-cli = {{ {cli_loc} }}
{backends}tokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\"] }}
serde = {{ version = \"1\", features = [\"derive\"] }}
"
        ),
    )
    .map_err(|err| io_err("write Cargo.toml", err))?;

    fs::write(
        dest.join("siderite.toml"),
        format!(
            "\
[app]
name = \"{name}\"

[server]
addr = \"127.0.0.1:8000\"

[databases.default]
url = \"{url}\"
"
        ),
    )
    .map_err(|err| io_err("write siderite.toml", err))?;

    fs::write(dest.join("src/lib.rs"), lib_rs(&ident))
        .map_err(|err| io_err("write src/lib.rs", err))?;
    fs::write(dest.join("src/main.rs"), main_rs(&ident))
        .map_err(|err| io_err("write src/main.rs", err))?;
    fs::write(dest.join("migrations/.gitkeep"), "")
        .map_err(|err| io_err("write migrations/.gitkeep", err))?;
    fs::write(dest.join(".gitignore"), "/target\n*.db\n.env\n")
        .map_err(|err| io_err("write .gitignore", err))?;
    fs::write(
        dest.join("README.md"),
        format!(
            "\
# {name}

```bash
siderite run
```

Open http://127.0.0.1:8000/ and http://127.0.0.1:8000/docs.
"
        ),
    )
    .map_err(|err| io_err("write README.md", err))?;
    Ok(())
}

fn lib_rs(ident: &str) -> String {
    format!(
        "\
//! {ident} HTTP API.

use siderite::prelude::*;

/// Greeting.
#[get(\"/\")]
async fn index() -> PlainText<&'static str> {{
    PlainText(\"Hello, siderite!\")
}}

/// The application.
pub fn app() -> App {{
    App::new()
        .title(\"{ident}\")
        .version(\"0.1.0\")
        .routes(routes![index])
}}
"
    )
}

fn main_rs(ident: &str) -> String {
    format!(
        "\
//! {ident} command line.

use siderite_cli::{{AppCli, CliSettings}};

#[tokio::main]
async fn main() -> std::process::ExitCode {{
    let settings = match siderite::config::load() {{
        Ok(settings) => CliSettings::from(&settings),
        Err(err) => {{
            eprintln!(\"{{err}}\");
            return std::process::ExitCode::from(1);
        }}
    }};
    AppCli::new({ident}::app)
        .settings(settings)
        .migrations_dir(\"migrations\")
        .run()
        .await
}}
"
    )
}

fn io_err(what: &str, err: std::io::Error) -> CliError {
    CliError::Io(format!("cannot {what}: {err}"))
}

fn print_help() {
    println!(
        "\
siderite new NAME

Write a new API crate in NAME/.

Options:
  --database sqlite|postgres|mysql   Default database (sqlite)
  --path DIR                         Path to a local siderite checkout
  --help                             Show this help
"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn parses_name_and_flags() {
        let (name, engine, path) = parse(&args(&[
            "new",
            "demo",
            "--database",
            "postgres",
            "--path",
            "/tmp/s",
        ]))
        .unwrap();
        assert_eq!(name, "demo");
        assert_eq!(engine, Engine::Postgres);
        assert_eq!(path, Some(PathBuf::from("/tmp/s")));
    }

    #[test]
    fn rejects_bad_names() {
        assert!(parse(&args(&["new"])).is_err());
        assert!(parse(&args(&["new", "Demo"])).is_err());
        assert!(parse(&args(&["new", "1demo"])).is_err());
    }

    #[test]
    fn writes_an_api_crate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let dir = std::env::temp_dir().join(format!(
            "siderite-new-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_project(
            &dir.join("demo_app"),
            "demo_app",
            Engine::Sqlite,
            Some(&root),
        )
        .unwrap();
        let cargo = std::fs::read_to_string(dir.join("demo_app/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"demo_app\""));
        assert!(cargo.contains("siderite-cli"));
        assert!(dir.join("demo_app/src/lib.rs").is_file());
        assert!(dir.join("demo_app/src/main.rs").is_file());
        assert!(dir.join("demo_app/siderite.toml").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }
}
