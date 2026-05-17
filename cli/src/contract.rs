use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use std::io::{self, Read};

#[derive(Debug, Args)]
pub struct ContractArgs {
    #[command(subcommand)]
    pub command: ContractCommand,
}

#[derive(Debug, Subcommand)]
pub enum ContractCommand {
    /// Validate a portable c-two.contract.v1 descriptor.
    Validate(ValidateArgs),
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Descriptor JSON path, or "-" to read from stdin.
    pub path: String,
}

pub fn run(args: ContractArgs) -> Result<()> {
    match args.command {
        ContractCommand::Validate(args) => validate(&args.path),
    }
}

fn validate(path: &str) -> Result<()> {
    let payload = read_payload(path)?;
    c2_contract::validate_portable_contract_descriptor_json(payload.as_bytes())
        .map_err(|err| anyhow!("{err}"))?;
    let digest = c2_contract::contract_descriptor_sha256_hex(payload.as_bytes())
        .map_err(|err| anyhow!("{err}"))?;
    let label = if path == "-" { "stdin" } else { path };
    println!("{label}: valid c-two.contract.v1 sha256={digest}");
    Ok(())
}

fn read_payload(path: &str) -> Result<String> {
    if path == "-" {
        let mut payload = String::new();
        io::stdin()
            .read_to_string(&mut payload)
            .map_err(|err| anyhow!("failed to read descriptor from stdin: {err}"))?;
        return Ok(payload);
    }
    std::fs::read_to_string(path)
        .map_err(|err| anyhow!("failed to read descriptor from {path}: {err}"))
}
