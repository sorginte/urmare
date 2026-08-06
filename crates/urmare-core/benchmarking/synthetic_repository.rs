use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MINIMUM_FILE_COUNT: usize = 4;

/// Metadata for one deterministic generated Python repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticRepository {
    pub changed_file: PathBuf,
    pub python_files: usize,
    pub source_modules: usize,
    pub test_files: usize,
}

/// Generates a chain-shaped Python repository with a conventional `src/` layout.
///
/// Ten percent of the requested files are pytest files. Every production
/// module ultimately depends on `module_00000`, and every test imports one of
/// those modules, producing a deliberately large but explainable blast radius.
/// The destination must be absent or empty; this helper never deletes files.
pub fn generate(root: &Path, python_files: usize) -> io::Result<SyntheticRepository> {
    if python_files < MINIMUM_FILE_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("synthetic repositories require at least {MINIMUM_FILE_COUNT} Python files"),
        ));
    }
    prepare_destination(root)?;

    let test_files = (python_files / 10).max(1);
    let source_modules = python_files - test_files - 1;
    let source_directory = root.join("src/generated");
    let test_directory = root.join("tests/generated");
    fs::create_dir_all(&source_directory)?;
    fs::create_dir_all(&test_directory)?;
    fs::write(
        root.join("pyproject.toml"),
        "[tool.urmare]\nsource-roots = [\"src\"]\n",
    )?;
    fs::write(
        source_directory.join("__init__.py"),
        "\"\"\"Synthetic benchmark package.\"\"\"\n",
    )?;

    for index in 0..source_modules {
        let path = source_directory.join(module_filename(index));
        let source = if index == 0 {
            "def value() -> int:\n    return 0\n".to_owned()
        } else {
            format!(
                "from generated import module_{previous:05}\n\n\ndef value() -> int:\n    return module_{previous:05}.value() + 1\n",
                previous = index - 1,
            )
        };
        fs::write(path, source)?;
    }

    for test_index in 0..test_files {
        let module_index = test_index * source_modules / test_files;
        let source = format!(
            "from generated import module_{module_index:05}\n\n\ndef test_value() -> None:\n    assert module_{module_index:05}.value() >= 0\n",
        );
        fs::write(
            test_directory.join(format!("test_module_{module_index:05}.py")),
            source,
        )?;
    }

    Ok(SyntheticRepository {
        changed_file: PathBuf::from("src/generated/module_00000.py"),
        python_files,
        source_modules,
        test_files,
    })
}

fn prepare_destination(root: &Path) -> io::Result<()> {
    match fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().transpose()?.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("destination `{}` must be empty", root.display()),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(root)?,
        Err(error) => return Err(error),
    }
    Ok(())
}

fn module_filename(index: usize) -> String {
    format!("module_{index:05}.py")
}
