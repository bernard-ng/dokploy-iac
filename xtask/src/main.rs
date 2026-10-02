use std::process::ExitCode;

use xtask::{
    CodegenMode, ProcessGenerator, repository_imperative_paths, repository_paths, repository_root,
    run_codegen, run_goldens_check, run_goldens_extract, run_imperative_codegen, run_specs_check,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Err(
            "usage: cargo xtask codegen [--check] | specs --check | goldens [--check]".into(),
        );
    };
    if command == "specs" {
        return run_specs(arguments);
    }
    if command == "goldens" {
        return run_goldens(arguments);
    }
    if command != "codegen" {
        return Err(format!(
            "unknown xtask command `{command}`; expected `codegen`, `specs`, or `goldens`"
        )
        .into());
    }

    let mode = match arguments.next().as_deref() {
        None => CodegenMode::Write,
        Some("--check") => CodegenMode::Check,
        Some(argument) => return Err(format!("unknown codegen argument `{argument}`").into()),
    };
    if let Some(argument) = arguments.next() {
        return Err(format!("unexpected argument `{argument}`").into());
    }

    let root = repository_root();
    let paths = repository_paths(&root);
    let generator = ProcessGenerator::new("oas3-gen", &root);
    let report = run_codegen(&generator, &paths, mode)?;
    let imperative_paths = repository_imperative_paths(&root);
    let imperative_report = run_imperative_codegen(&imperative_paths, mode)?;
    let verb = match mode {
        CodegenMode::Write => "generated",
        CodegenMode::Check => "verified",
    };
    println!(
        "{verb} {} operations across {} files ({} auth references normalized, {} known generator warnings)",
        report.operation_count,
        report.file_count,
        report.normalized_security_references,
        report.warning_count
    );
    println!(
        "{verb} {} imperative commands across {} resources with {} generated inputs",
        imperative_report.operation_count,
        imperative_report.resource_count,
        imperative_report.input_count
    );

    Ok(())
}

fn run_specs(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match arguments.next().as_deref() {
        Some("--check") => {}
        _ => return Err("usage: cargo xtask specs --check".into()),
    }
    if let Some(argument) = arguments.next() {
        return Err(format!("unexpected argument `{argument}`").into());
    }
    let report = run_specs_check(&repository_root())?;
    print!("{}", report.table);
    if report.failures.is_empty() {
        println!("specs ok");
        Ok(())
    } else {
        for failure in &report.failures {
            eprintln!("  - {failure}");
        }
        Err(format!("{} spec problem(s)", report.failures.len()).into())
    }
}

fn run_goldens(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let check = match arguments.next().as_deref() {
        None => false,
        Some("--check") => true,
        Some(argument) => return Err(format!("unknown goldens argument `{argument}`").into()),
    };
    if let Some(argument) = arguments.next() {
        return Err(format!("unexpected argument `{argument}`").into());
    }
    let root = repository_root();
    if !check {
        let report = run_goldens_extract(&root)?;
        println!(
            "mined {} scenarios into {} ledger files ({} new)",
            report.scenarios, report.files, report.added
        );
        return Ok(());
    }
    let report = run_goldens_check(&root)?;
    print!("{}", report.table);
    if report.failures.is_empty() {
        println!("goldens ok");
        Ok(())
    } else {
        for failure in &report.failures {
            eprintln!("  - {failure}");
        }
        Err(format!("{} golden ledger problem(s)", report.failures.len()).into())
    }
}
