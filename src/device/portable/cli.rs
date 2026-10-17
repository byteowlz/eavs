//! Thin CLI over shared operations; no runtime serving-route mutation.
use super::{
    apply, apply_bindings, connectivity, decode, observe, parse, parse_bindings, plan, preview,
    preview_bindings, read_file, revision, Binding, Bindings, Declaration, Observations, Preview,
};
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum Command {
    /// Validate and inspect only the explicitly named declaration file.
    Inspect {
        #[arg(long)]
        declaration: PathBuf,
    },
    /// Preview both current and candidate revisions; writes nothing.
    Preview(Publication),
    /// Publish reviewed declaration bytes with both CAS pins and --yes.
    Apply {
        #[command(flatten)]
        files: Publication,
        #[arg(long)]
        expected_revision: String,
        #[arg(long)]
        yes: bool,
    },
    /// Preview/apply a machine-local env-reference binding, never a credential value.
    Bind(Bind),
    /// Explicit HEAD-only transport observations; stdout only, no credentials/inference.
    Observe {
        #[arg(long)]
        declaration: PathBuf,
        #[arg(long, required = true)]
        provider: Vec<String>,
        #[arg(long, default_value_t = 30)]
        ttl_seconds: u64,
    },
    /// Honest selection plan only: no serving changes, paid fallback or replay.
    Plan {
        #[arg(long)]
        declaration: PathBuf,
        #[arg(long)]
        profile: String,
        #[arg(long, required = true)]
        route: Vec<String>,
        #[arg(long)]
        observations: Option<PathBuf>,
    },
}
#[derive(Args)]
pub struct Publication {
    #[arg(long)]
    pub candidate: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
}
#[derive(Args)]
pub struct Bind {
    #[arg(long)]
    pub declaration: PathBuf,
    /// Required pin on apply, from binding preview; declarations are not authority.
    #[arg(long)]
    pub declaration_revision: Option<String>,
    /// Explicit separate machine-local binding file; exclude from distribution.
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long)]
    pub provider: String,
    #[arg(long)]
    pub env: String,
    #[arg(long)]
    pub expected_revision: Option<String>,
    #[arg(long)]
    pub yes: bool,
}
fn load(path: &Path) -> Result<(Declaration, Vec<u8>)> {
    let bytes = read_file(path)?;
    Ok((parse(&bytes)?, bytes))
}
fn publish(
    files: &Publication,
    expected: Option<String>,
    candidate_pin: Option<&str>,
    yes: bool,
) -> Result<serde_json::Value> {
    let bytes = read_file(&files.candidate)?;
    let review = preview(&files.output, &bytes)?;
    if let Some(expected_revision) = expected {
        if !yes {
            bail!("apply requires --yes");
        }
        let approved = Preview {
            expected_revision,
            candidate_revision: candidate_pin
                .ok_or_else(|| anyhow::anyhow!("apply requires --candidate-revision"))?
                .into(),
        };
        apply(&files.output, &bytes, &approved)?;
    } else if yes {
        bail!("apply requires --expected-revision");
    }
    Ok(
        serde_json::json!({"schema_version":1,"applied":yes,"review":review,"serving_changed":false}),
    )
}
fn bind(args: Bind, candidate_pin: Option<&str>) -> Result<serde_json::Value> {
    let (d, declaration_bytes) = load(&args.declaration)?;
    if args.declaration == args.output
        || (args.output.exists()
            && std::fs::canonicalize(&args.declaration)? == std::fs::canonicalize(&args.output)?)
    {
        bail!("bindings require a separate machine-local file");
    }
    let before = read_file(&args.output)?;
    let mut bindings = if args.output.exists() {
        parse_bindings(&before, &d)?
    } else {
        Bindings {
            version: 1,
            bindings: Vec::new(),
        }
    };
    bindings.bindings.retain(|b| b.provider != args.provider);
    bindings.bindings.push(Binding {
        provider: args.provider,
        env: args.env,
    });
    let candidate = serde_json::to_vec_pretty(&bindings)?;
    let review = preview_bindings(&args.output, &candidate, &d)?;
    if review.expected_revision != revision(&before) {
        bail!("binding file changed; preview again");
    }
    let declaration_revision = revision(&declaration_bytes);
    if args.yes {
        if args.declaration_revision.as_deref() != Some(declaration_revision.as_str()) {
            bail!("declaration changed or unapproved; preview again");
        }
        let approved = Preview {
            expected_revision: args
                .expected_revision
                .ok_or_else(|| anyhow::anyhow!("binding apply requires --expected-revision"))?,
            candidate_revision: candidate_pin
                .ok_or_else(|| anyhow::anyhow!("binding apply requires --candidate-revision"))?
                .into(),
        };
        if read_file(&args.declaration)? != declaration_bytes {
            bail!("declaration changed; preview again");
        }
        apply_bindings(&args.output, &candidate, &approved, &d)?;
    }
    Ok(
        serde_json::json!({"schema_version":1,"applied":args.yes,"review":review,"declaration_revision":declaration_revision,"bindings":bindings,"serving_changed":false}),
    )
}
pub async fn execute(command: Command, candidate_pin: Option<&str>) -> Result<serde_json::Value> {
    match command {
        Command::Inspect { declaration } => Ok(
            serde_json::json!({"schema_version":1,"declaration":load(&declaration)?.0,"serving_changed":false}),
        ),
        Command::Preview(files) => publish(&files, None, candidate_pin, false),
        Command::Apply {
            files,
            expected_revision,
            yes,
        } => publish(&files, Some(expected_revision), candidate_pin, yes),
        Command::Bind(args) => bind(args, candidate_pin),
        Command::Observe {
            declaration,
            provider,
            ttl_seconds,
        } => {
            let d = load(&declaration)?.0;
            Ok(serde_json::to_value(
                observe(&d, &provider, ttl_seconds).await?,
            )?)
        }
        Command::Plan {
            declaration,
            profile,
            route,
            observations,
        } => {
            let d = load(&declaration)?.0;
            let observations = match observations {
                Some(path) => decode(&read_file(&path)?)?,
                None => Observations::empty(&d)?,
            };
            let selections = plan(&d, &profile, &route, &observations, connectivity::now()?)?;
            Ok(
                serde_json::json!({"schema_version":1,"kind":"declaration_selection_only","profile":profile,"selections":selections,"serving_changed":false,"authority_granted":false}),
            )
        }
    }
}
