//! Runs one named mock scenario through the native provider and the real FX executor and
//! prints the terminal outcome in FX's own wire encoding, the same encoding the Node host
//! reports. `host/node/conformance/native-parity.mjs` compares the two.
//!
//!   native_parity <openai|anthropic> <base_url> <scenario> [budget_ms]
//!
//! Offline: the base URL is a local mock server and the credential is a fixed fake.

use std::process::ExitCode;
use std::time::Duration;

use fx_core::*;
use fx_native_provider::{Anthropic, AnthropicConfig, OpenAiCompatible, OpenAiConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;

/// The same fake key the Node conformance harness uses.
const KEY: &str = "sk-CONFORMANCE-KEY-0123456789abcdef";

struct Quiet;

impl fx_core::Host for Quiet {
    fn cancel_requested(&mut self) -> bool {
        false
    }
    fn on_event(&mut self, _: &Event) -> Flow {
        Flow::Continue
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: native_parity <openai|anthropic> <base_url> <scenario> [budget_ms]");
        return ExitCode::from(2);
    }
    let budget = args
        .get(4)
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| AttemptBudget(Duration::from_millis(ms)));
    let request = Request {
        model: ModelRef(args[3].clone()),
        messages: vec![Message {
            role: Role::User,
            parts: vec![Part::Text("say hi".to_owned())],
            cache: CacheHint::Default,
        }],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        options: GenerationOptions::default(),
        correlation: Correlation {
            request_id: RequestId([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]),
            affinity_key: None,
        },
        budget,
    };
    let report = match args[1].as_str() {
        "openai" => {
            let config = match OpenAiConfig::new(&args[2], Secret::new(KEY)) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(2);
                }
            };
            let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
            execute(&mut capability, &request, &Limits::DEFAULT, &mut Quiet)
        }
        "anthropic" => {
            let config = match AnthropicConfig::new(&args[2], Secret::new(KEY)) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(2);
                }
            };
            let mut capability = SingleAttemptCapability::new(Anthropic::new(config));
            execute(&mut capability, &request, &Limits::DEFAULT, &mut Quiet)
        }
        other => {
            eprintln!("unknown provider {other}");
            return ExitCode::from(2);
        }
    };
    println!(
        "{}",
        String::from_utf8_lossy(&wire::encode_outcome(&report.outcome))
    );
    ExitCode::SUCCESS
}
