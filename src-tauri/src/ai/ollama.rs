//! The Ollama client and the dispatcher that keeps it off the tick loop
//! (PRD §3.1, §5.8).
//!
//! ## Why deliberation is dispatched rather than awaited
//!
//! §5.6 targets 1–2s per tick in Observe with a budget of 4–8 calls. That
//! assumes a GPU. Measured on this machine — ARM64, CPU-only Ollama — a single
//! short structured completion costs 3.2s on qwen3:1.7b and 16.2s on 8b, and
//! throughput is *flat* from one concurrent request to six: the model is
//! compute-bound on the same cores, so concurrency buys nothing. Six calls a
//! tick is therefore twenty seconds of wall clock however they are arranged.
//!
//! Blocking the tick on that makes Observe unwatchable, so calls are dispatched
//! to worker threads and collected in a later tick. The creature is not left
//! standing: it takes a Tier 1 plan immediately — which is exactly the
//! guarantee §5.2 gives Tier 1 — and adopts the model's plan when it arrives,
//! if that plan is still legal.
//!
//! **This is a deviation and it costs something.** A model plan is issued
//! against a world a few ticks old, so its first step is re-validated at
//! adoption rather than only at issue (§5.5). Deep mode keeps the synchronous
//! path, because studying one tick closely is exactly when the wall clock does
//! not matter.
//!
//! Prompt order is not cosmetic: the static rules go in the system message and
//! the creature's state in the user message, so Ollama's prefix cache covers
//! everything the calls share. Measured at M0: 3.82s → 0.58s of prompt
//! evaluation on qwen3:1.7b.

use crate::ai::schema::ActionMenu;
use crate::config::LlmConfig;
use serde::Deserialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How hard the model is asked to think (§5.4). Depth is a real wall-clock
/// lever on a reasoning model, so it compounds with the frequency saving rather
/// than duplicating it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Shallow,
    Standard,
    Deep,
}

impl Depth {
    pub fn as_str(self) -> &'static str {
        match self {
            Depth::Shallow => "shallow",
            Depth::Standard => "standard",
            Depth::Deep => "deep",
        }
    }

    /// qwen3 exposes reasoning as a switch. Only the deepest calls get it: a
    /// thinking pass on this hardware costs more than the rest of the call.
    fn thinking(self) -> bool {
        matches!(self, Depth::Deep)
    }

    /// Response token budget.
    ///
    /// Raised roughly 2.5x at M6 after measuring that the old budgets were the
    /// main cause of unusable answers rather than the models being incapable.
    /// qwen3 writes a short preamble before its JSON even with `think` off, and
    /// a budget that runs out mid-object arrives as NO_JSON_IN_RESPONSE —
    /// indistinguishable, from the log, from a model that simply failed.
    ///
    /// Measured on qwen3:4b, eight calls each:
    ///
    ///   num_predict 160    17% usable   mean 8,811ms
    ///   num_predict 512   100% usable   mean 7,613ms
    ///
    /// Note the latency went *down*. A truncated call spends its entire budget
    /// and returns nothing, so a budget too small to finish is the most
    /// expensive setting available — it pays full price for a fallback.
    fn num_predict(self) -> u32 {
        match self {
            Depth::Shallow => 256,
            Depth::Standard => 384,
            Depth::Deep => 768,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CallError {
    /// The endpoint could not be reached at all.
    Unreachable(String),
    Timeout,
    /// A response arrived but was not a shape we understand.
    BadResponse(String),
    Status(u16),
}

impl CallError {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallError::Unreachable(_) => "OLLAMA_UNREACHABLE",
            CallError::Timeout => "OLLAMA_TIMEOUT",
            CallError::BadResponse(_) => "OLLAMA_BAD_RESPONSE",
            CallError::Status(_) => "OLLAMA_HTTP_ERROR",
        }
    }
}

/// One assembled prompt, split so the shared half can be cached.
#[derive(Debug, Clone)]
pub struct Prompt {
    /// Identical across every call: rules, vocabulary, output format.
    pub system: String,
    /// This creature, this tick.
    pub user: String,
}

impl Prompt {
    pub fn full_text(&self) -> String {
        format!("{}\n\n---\n\n{}", self.system, self.user)
    }

    /// A stable hash of the whole prompt, for `decisions.prompt_hash`. Lets a
    /// run be grouped by prompt shape without reading megabytes of text.
    pub fn hash(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.system.hash(&mut h);
        self.user.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub raw: String,
    pub latency_ms: u64,
    pub prompt_tokens: u32,
    pub response_tokens: u32,
    /// Tokens the server did not have to re-evaluate because the prefix was
    /// already cached. The whole reason the static half goes first.
    pub cached_tokens: u32,
}

#[derive(Deserialize)]
struct ChatReply {
    message: ChatMessage,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
    #[serde(default)]
    prompt_eval_duration: u64,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

/// A blocking client. One per worker thread.
pub struct Client {
    agent: ureq::Agent,
    cfg: LlmConfig,
}

impl Client {
    pub fn new(cfg: LlmConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_millis(2_000))
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build();
        Self { agent, cfg }
    }

    /// One call. Retries are the caller's business (§5.8 allows exactly one,
    /// with a repair instruction).
    pub fn chat(
        &self,
        prompt: &Prompt,
        schema: &serde_json::Value,
        depth: Depth,
    ) -> Result<Response, CallError> {
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": prompt.system },
                { "role": "user", "content": prompt.user },
            ],
            "stream": false,
            // Structured output: the model is constrained at generation time
            // rather than corrected afterwards, which removes most of what §5.8
            // would otherwise spend its one retry on.
            "format": schema,
            "think": depth.thinking(),
            "options": {
                "temperature": self.cfg.temperature,
                "num_predict": self.cfg.num_predict_override.unwrap_or_else(|| depth.num_predict()),
                "num_ctx": self.cfg.num_ctx,
            }
        });

        let started = Instant::now();
        let url = format!("{}/api/chat", self.cfg.endpoint.trim_end_matches('/'));
        let reply = self.agent.post(&url).send_json(body);
        let latency_ms = started.elapsed().as_millis() as u64;

        let reply = match reply {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => return Err(CallError::Status(code)),
            Err(ureq::Error::Transport(t)) => {
                // ureq reports a timeout as a transport error; the distinction
                // matters because a timeout is a slow model and unreachable is
                // a missing one, and they call for different responses.
                let msg = t.to_string();
                return Err(if msg.contains("timed out") || msg.contains("timeout") {
                    CallError::Timeout
                } else {
                    CallError::Unreachable(msg)
                });
            }
        };

        let parsed: ChatReply = reply
            .into_json()
            .map_err(|e| CallError::BadResponse(e.to_string()))?;

        // Ollama does not report cache hits directly, and it does *not* reduce
        // `prompt_eval_count` when it reuses a prefix — the count is the whole
        // prompt either way. The only signal is the rate.
        //
        // Measured on this hardware against qwen3:8b, same 1,105-token system
        // prefix, different user message:
        //
        //   cold   1,105 tokens in 13.19s     84 tokens/sec
        //   warm   1,108 tokens in  0.40s  2,770 tokens/sec
        //
        // The previous test asked for a prompt evaluation under one
        // millisecond, which a cached prefix still misses by a factor of four
        // hundred, so it reported 0% cache hits on every call ever made and the
        // §5.7 prefix ordering looked inert when it was working.
        const CACHED_TOKENS_PER_SEC: f64 = 500.0;
        let secs = parsed.prompt_eval_duration as f64 / 1e9;
        let rate = if secs > 0.0 { parsed.prompt_eval_count as f64 / secs } else { f64::MAX };
        let cached_tokens = if rate > CACHED_TOKENS_PER_SEC { parsed.prompt_eval_count } else { 0 };

        Ok(Response {
            raw: parsed.message.content,
            latency_ms,
            prompt_tokens: parsed.prompt_eval_count,
            response_tokens: parsed.eval_count,
            cached_tokens,
        })
    }
}

// ------------------------------------------------------------- dispatching

/// A deliberation asked for, not yet answered.
pub struct Request {
    pub creature_id: i64,
    pub issued_tick: i64,
    pub prompt: Prompt,
    pub menu: ActionMenu,
    pub depth: Depth,
    pub schema: serde_json::Value,
    /// True when a crisis bought this call at a discount (§5.5).
    pub crisis_exempt: bool,
}

/// A deliberation answered, waiting to be applied.
pub struct Completion {
    pub creature_id: i64,
    pub issued_tick: i64,
    pub menu: ActionMenu,
    pub depth: Depth,
    pub crisis_exempt: bool,
    pub prompt: Prompt,
    pub result: Result<Response, CallError>,
    /// Set when the first answer was unusable and a repair retry was spent.
    pub repaired: bool,
}

/// Worker threads and the two queues between them and the simulation.
pub struct Dispatcher {
    to_workers: Option<Sender<Request>>,
    from_workers: Receiver<Completion>,
    workers: Vec<std::thread::JoinHandle<()>>,
    outstanding: Arc<AtomicUsize>,
    capacity: usize,
}

impl Dispatcher {
    pub fn new(cfg: &LlmConfig, schema_for_repair: serde_json::Value) -> Self {
        // Concurrency is kept configurable for hardware that benefits from it.
        // On a CPU-only host it does not: measured throughput is flat from one
        // worker to six, because they contend for the same cores.
        let n = cfg.max_concurrent.max(1) as usize;
        let (to_workers, work_rx) = std::sync::mpsc::channel::<Request>();
        let (done_tx, from_workers) = std::sync::mpsc::channel::<Completion>();
        let work_rx = Arc::new(std::sync::Mutex::new(work_rx));
        let outstanding = Arc::new(AtomicUsize::new(0));

        let mut workers = Vec::with_capacity(n);
        for i in 0..n {
            let work_rx = work_rx.clone();
            let done_tx = done_tx.clone();
            let cfg = cfg.clone();
            let outstanding = outstanding.clone();
            let repair_schema = schema_for_repair.clone();
            let handle = std::thread::Builder::new()
                .name(format!("life-zone-llm-{i}"))
                .spawn(move || {
                    let client = Client::new(cfg);
                    loop {
                        // Held only across `recv`, never across the call, so
                        // workers do not serialise on each other.
                        let req = {
                            let guard = match work_rx.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            match guard.recv() {
                                Ok(r) => r,
                                Err(_) => break,
                            }
                        };

                        let mut repaired = false;
                        let mut result = client.chat(&req.prompt, &req.schema, req.depth);

                        // §5.8: one retry with a repair instruction. Only for a
                        // reply that arrived and was unusable — retrying a
                        // timeout just spends the budget twice.
                        if let Ok(ref r) = result {
                            if crate::ai::schema::extract_json(&r.raw).is_none() {
                                repaired = true;
                                let mut repair = req.prompt.clone();
                                repair.user.push_str(
                                    "\n\nYour previous answer was not valid JSON. \
                                     Reply with the JSON object only, nothing else.",
                                );
                                if let Ok(second) = client.chat(&repair, &repair_schema, req.depth)
                                {
                                    result = Ok(second);
                                }
                            }
                        }

                        outstanding.fetch_sub(1, Ordering::SeqCst);
                        if done_tx
                            .send(Completion {
                                creature_id: req.creature_id,
                                issued_tick: req.issued_tick,
                                menu: req.menu,
                                depth: req.depth,
                                crisis_exempt: req.crisis_exempt,
                                prompt: req.prompt,
                                result,
                                repaired,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .expect("spawning an LLM worker");
            workers.push(handle);
        }

        Self {
            to_workers: Some(to_workers),
            from_workers,
            workers,
            outstanding,
            // Exactly as deep as there are workers, plus one in hand.
            //
            // Queue depth is staleness. Measured with a depth of three: an
            // end-to-end round trip of 231 ticks against an 8s call, because a
            // request spent most of its life waiting to start. A creature lives
            // 672 ticks; a deliberation that takes a third of that to come back
            // is answering a question from another era. A shallow queue makes
            // the hardware's real capacity visible instead of hiding it as
            // latency.
            capacity: n + 1,
        }
    }

    pub fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::SeqCst)
    }

    pub fn has_room(&self) -> bool {
        self.outstanding() < self.capacity
    }

    /// Queue a call. Returns false if there is no room, in which case the
    /// creature simply keeps its Tier 1 plan.
    pub fn dispatch(&self, req: Request) -> bool {
        if !self.has_room() {
            return false;
        }
        let Some(tx) = self.to_workers.as_ref() else {
            return false;
        };
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        if tx.send(req).is_err() {
            self.outstanding.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Everything answered since the last call.
    ///
    /// Sorted by creature id before being handed back: completion order
    /// depends on which worker finished first, and while the model's *content*
    /// is legitimately non-deterministic (invariant 7), the order in which the
    /// engine applies a given set of answers should not be.
    pub fn collect(&self) -> Vec<Completion> {
        let mut out = Vec::new();
        while let Ok(c) = self.from_workers.try_recv() {
            out.push(c);
        }
        out.sort_by_key(|c| (c.creature_id, c.issued_tick));
        out
    }

    /// Block until one answer arrives, for Deep mode's synchronous path.
    pub fn wait_one(&self, timeout: Duration) -> Option<Completion> {
        self.from_workers.recv_timeout(timeout).ok()
    }

    /// Stop the workers and wait for them. Dropping the sender is what tells
    /// them to finish.
    pub fn shutdown(&mut self) {
        self.to_workers = None;
        for h in self.workers.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LlmConfig {
        LlmConfig { endpoint: "http://127.0.0.1:1".into(), timeout_ms: 300, ..Default::default() }
    }

    #[test]
    fn a_prompt_hashes_stably_and_differs_when_the_creature_does() {
        let a = Prompt { system: "rules".into(), user: "Ansa is hungry".into() };
        let b = Prompt { system: "rules".into(), user: "Ansa is hungry".into() };
        let c = Prompt { system: "rules".into(), user: "Brel is hungry".into() };
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), c.hash());
        assert!(a.full_text().contains("rules") && a.full_text().contains("Ansa"));
    }

    #[test]
    fn an_unreachable_endpoint_is_an_error_and_not_a_panic() {
        // The tick must survive Ollama not being there at all.
        let client = Client::new(cfg());
        let prompt = Prompt { system: "s".into(), user: "u".into() };
        let err = client
            .chat(&prompt, &serde_json::json!({}), Depth::Shallow)
            .expect_err("nothing is listening on port 1");
        assert!(matches!(err, CallError::Unreachable(_) | CallError::Timeout));
        assert!(!err.as_str().is_empty());
    }

    #[test]
    fn the_dispatcher_bounds_its_own_queue() {
        // A deliberation that waits ten ticks to start is answering a question
        // the creature stopped asking.
        let mut c = cfg();
        c.max_concurrent = 1;
        let d = Dispatcher::new(&c, serde_json::json!({}));

        let make = || Request {
            creature_id: 1,
            issued_tick: 0,
            prompt: Prompt { system: "s".into(), user: "u".into() },
            menu: ActionMenu::default(),
            depth: Depth::Shallow,
            schema: serde_json::json!({}),
            crisis_exempt: false,
        };

        let mut accepted = 0;
        for _ in 0..20 {
            if d.dispatch(make()) {
                accepted += 1;
            }
        }
        assert!(accepted <= 4, "queue should be bounded, accepted {accepted}");
    }

    #[test]
    fn failed_calls_still_come_back_so_the_creature_is_never_left_waiting() {
        let mut c = cfg();
        c.max_concurrent = 1;
        let d = Dispatcher::new(&c, serde_json::json!({}));
        d.dispatch(Request {
            creature_id: 7,
            issued_tick: 3,
            prompt: Prompt { system: "s".into(), user: "u".into() },
            menu: ActionMenu::default(),
            depth: Depth::Shallow,
            schema: serde_json::json!({}),
            crisis_exempt: false,
        });

        let got = d.wait_one(Duration::from_secs(10)).expect("a completion, even a failed one");
        assert_eq!(got.creature_id, 7);
        assert_eq!(got.issued_tick, 3);
        assert!(got.result.is_err());
        assert_eq!(d.outstanding(), 0, "the slot is released");
    }

    #[test]
    fn depth_controls_what_the_call_actually_costs() {
        assert!(!Depth::Shallow.thinking(), "reasoning is the expensive part");
        assert!(Depth::Deep.thinking());
        assert!(Depth::Shallow.num_predict() < Depth::Deep.num_predict());
    }
}
