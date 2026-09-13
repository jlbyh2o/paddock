//! Cheap checks that run FreeToken's own code before committing to a long job.
//!
//! Both the checkpoint conversion and a chat template can fail in ways no amount of
//! inspecting files from the outside would predict — the authority on whether FreeToken
//! can read a checkpoint is FreeToken. Asking it costs one Python subprocess and a few
//! seconds, against a conversion that otherwise runs for minutes and writes tens of
//! gigabytes before raising.

use std::path::{Path, PathBuf};

use super::locate::Freetoken;

/// How a check turned out. Three outcomes rather than two, because "it works but not
/// completely" is a real and common answer that a boolean would have to round the wrong
/// way.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok(String),
    /// Usable, with a caveat worth reading before continuing.
    Warn(String),
    Fail(String),
}

impl Outcome {
    pub fn detail(&self) -> &str {
        match self {
            Outcome::Ok(d) | Outcome::Warn(d) | Outcome::Fail(d) => d,
        }
    }

    pub fn is_fail(&self) -> bool {
        matches!(self, Outcome::Fail(_))
    }

    pub fn is_clean(&self) -> bool {
        matches!(self, Outcome::Ok(_))
    }

    /// Parse a script's last output line.
    pub fn parse(line: &str) -> Self {
        let line = line.trim();
        if let Some(d) = line.strip_prefix("OK ") {
            Outcome::Ok(d.to_string())
        } else if let Some(d) = line.strip_prefix("WARN ") {
            Outcome::Warn(d.to_string())
        } else if let Some(d) = line.strip_prefix("FAIL ") {
            Outcome::Fail(d.to_string())
        } else if line.is_empty() {
            Outcome::Fail("the check produced no output".into())
        } else {
            // Anything unrecognized is a failure, never a silent pass.
            Outcome::Fail(line.to_string())
        }
    }
}

/// The Python interpreter that can import `freetoken`, given a resolved CLI.
fn python_for(ft: &Freetoken) -> Option<PathBuf> {
    if ft.prefix.first().map(String::as_str) == Some("-m") {
        return Some(ft.program.clone());
    }
    let candidate = ft.program.parent()?.join("python");
    super::locate::is_executable(&candidate).then_some(candidate)
}

/// Build the argv for a check. `None` when there is no interpreter to run it with.
fn command(ft: &Freetoken, script: &str, args: &[String]) -> Option<Vec<String>> {
    let python = python_for(ft)?;
    let mut argv = vec![python.display().to_string(), "-c".into(), script.into()];
    argv.extend(args.iter().cloned());
    Some(argv)
}

/// Ask FreeToken what it makes of a checkpoint, before converting it.
pub fn convert_command(
    ft: &Freetoken,
    model_dir: &Path,
    moe_strategy: &str,
) -> Option<Vec<String>> {
    command(ft, CONVERT_SCRIPT, &[model_dir.display().to_string(), moe_strategy.to_string()])
}

/// List the model architectures FreeToken can load. Needs no checkpoint at all — the
/// registry is a static map keyed by architecture name.
pub fn architectures_command(ft: &Freetoken) -> Option<Vec<String>> {
    command(ft, ARCHITECTURES_SCRIPT, &[])
}

/// Render a candidate chat template against a checkpoint's real tokenizer.
pub fn template_command(ft: &Freetoken, model_dir: &Path, jinja: &Path) -> Option<Vec<String>> {
    command(ft, TEMPLATE_SCRIPT, &[model_dir.display().to_string(), jinja.display().to_string()])
}

/// Run a prepared check and parse its verdict.
pub async fn run(argv: Vec<String>, env: &[(String, String)]) -> Outcome {
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    match cmd.output().await {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            match text.lines().rev().find(|l| !l.trim().is_empty()) {
                Some(line) => Outcome::parse(line),
                // No stdout at all means the interpreter itself failed; the reason is on
                // stderr, and reporting "no output" would hide it.
                None => {
                    let err = String::from_utf8_lossy(&out.stderr);
                    Outcome::Fail(
                        err.lines()
                            .rev()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("the check produced no output")
                            .trim()
                            .to_string(),
                    )
                }
            }
        }
        Err(e) => Outcome::Fail(format!("could not run the check: {e}")),
    }
}

/// Resolves the checkpoint through FreeToken's own `EngineConfig` and compares what it
/// concluded against what the checkpoint declares.
///
/// The mismatch worth catching: a checkpoint that declares a quantization which FreeToken
/// then resolves to `expert_quant=none`. FreeToken reads every dialect through one
/// `QuantConfig` now, so this no longer fires for a whole export format the way it once
/// did for llm-compressor (see `docs/freetoken-compressed-tensors-moe.md`). What still
/// reaches it is a config wrong on its own terms — a modelopt `MIXED_PRECISION`
/// allow-list with no entry covering `.mlp.experts`. Cheap to ask, and the alternative is
/// finding out minutes into a conversion.
const CONVERT_SCRIPT: &str = r#"
import sys, json, os
model_dir = sys.argv[1]
moe_strategy = sys.argv[2] if len(sys.argv) > 2 else "offload"

def declared_quant(path):
    """What the checkpoint says about itself, wherever the key happens to live."""
    try:
        with open(os.path.join(path, "config.json"), encoding="utf-8") as f:
            raw = json.load(f)
    except Exception:
        return None
    for holder in (raw, raw.get("text_config") or {}):
        q = holder.get("quantization_config") or {}
        value = q.get("format") or q.get("quant_algo") or q.get("quant_method")
        if value:
            return str(value)
    return None

def run():
    import torch
    from freetoken.engine.config import EngineConfig
    from freetoken.distributed import DistributedInfo, set_tp_info, try_get_tp_info
    if try_get_tp_info() is None:
        set_tp_info(rank=0, size=1)
    cfg = EngineConfig(model_path=model_dir, tp_info=DistributedInfo(0, 1),
                       dtype=torch.bfloat16, moe_strategy=moe_strategy)
    mc = cfg.model_config
    is_moe = bool(getattr(mc, "is_moe", False))
    experts = int(getattr(mc, "num_experts", 0) or 0)
    layers = int(getattr(mc, "num_moe_layers", 0) or 0)
    quant = str(getattr(mc, "expert_quant", "none") or "none")
    arch = (getattr(mc, "architectures", None) or ["?"])[0]

    if is_moe:
        summary = "%s: MoE, %d experts x %d layers, expert_quant=%s" % (
            arch, experts, layers, quant)
    else:
        summary = "%s: dense" % arch

    declared = declared_quant(model_dir)
    if is_moe and quant == "none" and declared:
        return ("WARN the checkpoint declares %s but FreeToken resolved its experts as "
                "unquantized (expert_quant=none). If the expert tensors really are "
                "quantized, the loader rejects the checkpoint for disagreeing with its "
                "own quant config. %s" % (declared, summary))
    if is_moe and experts == 0:
        return "WARN %s -- no experts resolved; the expert pass has nothing to pack" % summary
    return "OK " + summary

try:
    line = run()
except Exception as exc:
    line = "FAIL %s: %s" % (type(exc).__name__, exc)
print(line)
sys.exit(1 if line.startswith("FAIL") else 0)
"#;

/// Prints FreeToken's registered architectures, one per line, after an `OK` count.
const ARCHITECTURES_SCRIPT: &str = r#"
import sys
try:
    from freetoken.models.register import _MODEL_REGISTRY as registry
    names = sorted(registry)
    if not names:
        raise RuntimeError("the model registry is empty")
    print("OK %d" % len(names))
    for name in names:
        print(name)
except Exception as exc:
    print("FAIL %s: %s" % (type(exc).__name__, exc))
    sys.exit(1)
"#;

/// Parse the architecture listing. The verdict line comes first, then one name per line.
pub fn parse_architectures(stdout: &str) -> Result<Vec<String>, String> {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("");
    if !first.starts_with("OK ") {
        return Err(Outcome::parse(first).detail().to_string());
    }
    let names: Vec<String> = lines.map(str::to_string).collect();
    if names.is_empty() {
        return Err("the registry listing was empty".into());
    }
    Ok(names)
}

/// Renders a conversation through a candidate template and prints its verdict.
///
/// The shapes are tried in order of coverage because templates disagree about tool
/// calls: some want `function.arguments` as a mapping and raise on a JSON string, others
/// want the string. Probing only one shape reports a perfectly good template as broken —
/// which is exactly what an earlier version of this script did to a checkpoint's own
/// template while passing the replacement, the most misleading outcome available.
const TEMPLATE_SCRIPT: &str = r#"
import sys, json
model_dir, jinja_path = sys.argv[1], sys.argv[2]

def run():
    from transformers import AutoTokenizer
    tok = AutoTokenizer.from_pretrained(model_dir)
    with open(jinja_path, encoding="utf-8") as f:
        tok.chat_template = f.read()

    tools = [{"type": "function", "function": {
        "name": "get_weather", "description": "Current weather for a city",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}},
                       "required": ["city"]}}}]
    base = [
        {"role": "system", "content": "You are a helpful assistant."},
        {"role": "user", "content": "What is the weather in Paris?"},
    ]
    def with_call(args):
        return base + [
            {"role": "assistant", "content": "", "tool_calls": [
                {"type": "function",
                 "function": {"name": "get_weather", "arguments": args}}]},
            {"role": "tool", "name": "get_weather", "content": "18C, clear"},
            {"role": "assistant", "content": "It is 18C and clear in Paris."},
            {"role": "user", "content": "And tomorrow?"},
        ]

    attempts = [
        ("tool calls", with_call({"city": "Paris"}), {"tools": tools}),
        ("tool calls", with_call(json.dumps({"city": "Paris"})), {"tools": tools}),
        ("tools listed", base, {"tools": tools}),
        ("plain chat", base, {}),
    ]

    errors = []
    for label, messages, kwargs in attempts:
        try:
            text = tok.apply_chat_template(
                messages, tokenize=False, add_generation_prompt=True, **kwargs)
        except Exception as exc:
            errors.append("%s: %s: %s" % (label, type(exc).__name__, exc))
            continue
        if not text or not text.strip():
            errors.append("%s: rendered an empty prompt" % label)
            continue
        detail = "%d chars, %d tokens (%s)" % (
            len(text), len(tok(text)["input_ids"]), label)
        if label == "tool calls":
            return "OK " + detail
        return "WARN %s; the tool-call form did not render -- %s" % (
            detail, errors[0] if errors else "unknown")
    return "FAIL " + (errors[-1] if errors else "nothing rendered")

try:
    line = run()
except Exception as exc:
    line = "FAIL %s: %s" % (type(exc).__name__, exc)
print(line)
sys.exit(1 if line.startswith("FAIL") else 0)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_parse_into_their_three_forms() {
        assert_eq!(
            Outcome::parse("OK Qwen3MoE: MoE, 128 experts x 48 layers, expert_quant=nvfp4"),
            Outcome::Ok("Qwen3MoE: MoE, 128 experts x 48 layers, expert_quant=nvfp4".into())
        );
        assert!(matches!(Outcome::parse("WARN the checkpoint declares ..."), Outcome::Warn(_)));
        assert_eq!(
            Outcome::parse("FAIL ValueError: nope"),
            Outcome::Fail("ValueError: nope".into())
        );
    }

    #[test]
    fn unrecognized_output_is_a_failure_not_a_pass() {
        assert!(Outcome::parse("").is_fail());
        assert!(Outcome::parse("Traceback (most recent call last):").is_fail());
        assert!(!Outcome::parse("Segmentation fault").is_clean());
    }

    #[test]
    fn only_a_clean_result_is_clean() {
        assert!(Outcome::Ok("x".into()).is_clean());
        assert!(!Outcome::Warn("x".into()).is_clean());
        assert!(!Outcome::Warn("x".into()).is_fail());
        assert!(Outcome::Fail("x".into()).is_fail());
    }

    #[test]
    fn the_architecture_listing_is_parsed_or_reported() {
        let out = "OK 3\nGptOssForCausalLM\nLlamaForCausalLM\nQwen3MoeForCausalLM\n";
        assert_eq!(
            parse_architectures(out).unwrap(),
            vec!["GptOssForCausalLM", "LlamaForCausalLM", "Qwen3MoeForCausalLM"]
        );
        assert!(parse_architectures("FAIL ImportError: no freetoken").is_err());
        assert!(parse_architectures("OK 0\n").is_err(), "a count with no names is not a list");
        assert!(parse_architectures("").is_err());
    }

    #[test]
    fn a_bare_binary_uses_the_interpreter_beside_it() {
        // A python-form CLI runs its own interpreter.
        let via_python = Freetoken {
            program: PathBuf::from("/venv/bin/python"),
            prefix: vec!["-m".into(), "freetoken.cli".into()],
            origin: "test".into(),
        };
        assert_eq!(python_for(&via_python), Some(PathBuf::from("/venv/bin/python")));

        // A bare `ft` with no sibling interpreter has nothing to run.
        let missing = Freetoken {
            program: PathBuf::from("/nonexistent/bin/ft"),
            prefix: Vec::new(),
            origin: "test".into(),
        };
        assert_eq!(python_for(&missing), None);
        assert!(convert_command(&missing, Path::new("/models/x"), "offload").is_none());
    }
}
