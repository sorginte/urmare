#[path = "../benchmarking/synthetic_repository.rs"]
mod synthetic_repository;

use std::env;
use std::error::Error;
use std::io;
use std::path::PathBuf;

use synthetic_repository::generate;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .and_then(|value| PathBuf::from(value).file_name().map(ToOwned::to_owned))
        .unwrap_or_else(|| "generate_synthetic".into());
    let destination = arguments.next().ok_or_else(|| usage_error(&program))?;
    let file_count = arguments.next().ok_or_else(|| usage_error(&program))?;
    if arguments.next().is_some() {
        return Err(usage_error(&program).into());
    }
    let file_count = file_count
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file count must be Unicode"))?
        .parse::<usize>()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("file count must be a positive integer: {error}"),
            )
        })?;
    let destination = PathBuf::from(destination);
    let repository = generate(&destination, file_count)?;

    println!(
        "Generated {} Python files ({} source modules, {} tests) under {}",
        repository.python_files,
        repository.source_modules,
        repository.test_files,
        destination.display(),
    );
    println!("Changed-file seed: {}", repository.changed_file.display());
    Ok(())
}

fn usage_error(program: &std::ffi::OsStr) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "usage: {} <empty-destination> <python-file-count>",
            PathBuf::from(program).display(),
        ),
    )
}
