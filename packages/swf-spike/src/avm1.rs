//! Tiny AVM1 interpreter — just enough to drive Dofus 1.29 spell SWFs.
//!
//! What it handles (every opcode actually emitted by spell 802 — verified via
//! `dump-avm1` against `clips/spells/802.swf`):
//!
//! ```text
//! Push, Pop, ConstantPool, GetVariable, SetVariable, GetMember,
//! GetProperty, SetProperty, RandomNumber, Add2, Subtract, Multiply,
//! GotoFrame2, Stop, Play, CallMethod, End
//! ```
//!
//! Plus the four MovieClip properties Dofus actually drives (`_xscale`,
//! `_yscale`, `_alpha`, `_rotation`), the `random(n)` builtin, target paths
//! (`""` = this, `"_parent"`, `"_root"`), and `_parent.removeMovieClip()`.
//!
//! What it does NOT handle (and will silently log a warning the first time
//! they appear): `If`, `Jump`, `DefineFunction*`, `NewObject`, `NewMethod`,
//! `Enumerate`, type coercions beyond Number/String, etc. If a spell hits one
//! of those it'll typically fall back to the unscripted timeline behavior,
//! which is at worst what `render-cast-sheet` already produces.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use swf::avm1::read::Reader;
use swf::avm1::types::{Action, Push, Value as PushValue};

/// Stable identifier for one MovieClip instance in the display tree. We use
/// the path `(parent_id, depth)` to derive these so that re-placing the same
/// clip on the same depth keeps its state across frames (matches Flash
/// semantics where a `PlaceObject` to an occupied depth modifies the existing
/// clip instead of creating a new one).
pub type InstanceId = u32;

/// Per-clip mutable runtime state. Scripts read and write these fields.
#[derive(Clone, Debug)]
pub struct ClipState {
    /// 1-indexed current frame, matching `_currentframe` semantics.
    pub current_frame: u16,
    /// Total number of timeline frames in the underlying DefineSprite.
    pub total_frames: u16,
    pub playing: bool,
    /// Degrees. 0 = no rotation. Default 0.
    pub rotation: f64,
    /// 0..100. Default 100.
    pub alpha: f64,
    /// 0..100. Default 100.
    pub xscale: f64,
    pub yscale: f64,
    /// `_x` and `_y` — pixels in parent coordinates (matrix.tx/ty / 20).
    /// Synced from the placement matrix's translation. Default 0.
    pub x: f64,
    pub y: f64,
    /// Local variables (`var`-less assignments and `onClipEvent(load)` locals).
    pub vars: HashMap<String, Value>,
    /// Parent instance id for `_parent` resolution.
    pub parent: Option<InstanceId>,
    /// True after `removeMovieClip` is called on this clip; the renderer
    /// honours it by skipping the clip on subsequent ticks.
    pub removed: bool,
    /// Set by `onClipEvent(load)` running once per instance.
    pub loaded: bool,
    /// FLASH SEMANTIC (verified against Ruffle's `apply_place_object`,
    /// display_object.rs:2497): once a script writes to `_x`/`_y`/
    /// `_xscale`/`_yscale`/`_rotation` (or any other transform-affecting
    /// property), this flag flips to `true` and all subsequent timeline
    /// `Modify`/`Place` ops on this clip's matrix become no-ops. The
    /// script-set transform persists across the clip's entire lifetime.
    /// This is what keeps spell 802's sp6._x at -3.75 (sp7 f2 value)
    /// even though sp7's f3, f4, … `Modify` ops would otherwise overwrite
    /// it every frame.
    pub transformed_by_script: bool,
}

impl ClipState {
    pub fn new(total_frames: u16, parent: Option<InstanceId>) -> Self {
        Self {
            current_frame: 1,
            total_frames,
            playing: true,
            rotation: 0.0,
            alpha: 100.0,
            xscale: 100.0,
            yscale: 100.0,
            x: 0.0,
            y: 0.0,
            vars: HashMap::new(),
            parent,
            removed: false,
            loaded: false,
            transformed_by_script: false,
        }
    }
}

/// A user-defined AVM1 function (from `DefineFunction`/`DefineFunction2`).
/// Stores the bytecode body so we can re-invoke it when the function is
/// called via `CallMethod`/`CallFunction`, or when it's bound to a clip
/// event property (`this.onEnterFrame = function(){…}`).
#[derive(Debug)]
pub struct FnDef {
    pub params: Vec<String>,
    pub code: Vec<u8>,
    pub swf_version: u8,
    /// Function body's source SWF bytecode. We store the bytes inline so the
    /// FnDef can outlive the parent action stream's lifetime.
    pub register_count: u8,
}

/// AVM1 dynamic value.
#[derive(Clone, Debug)]
pub enum Value {
    Undefined,
    Null,
    Number(f64),
    String(String),
    Bool(bool),
    /// Reference to another clip instance (e.g. `_parent` on the stack).
    Clip(InstanceId),
    /// User-defined function created by `DefineFunction`/`DefineFunction2`.
    Function(Rc<FnDef>),
    /// Object literal `{key:val,…}` from `InitObject`. Used by `attachMovie`
    /// init-object args (e.g. `attachMovie("frag","f"+c,c,{_x:_X,_y:_Y})`)
    /// and by general property bag patterns.
    Object(Rc<RefCell<HashMap<String, Value>>>),
}

impl Value {
    pub fn as_f64(&self) -> f64 {
        match self {
            Value::Number(n) => *n,
            Value::String(s) => s.parse().unwrap_or(f64::NAN),
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            _ => f64::NAN,
        }
    }
    pub fn as_string(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            Value::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e16 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Value::Undefined => "undefined".to_string(),
            Value::Null => "null".to_string(),
            Value::Clip(_) => "[movie clip]".to_string(),
            Value::Function(_) => "[function]".to_string(),
            Value::Object(_) => "[object]".to_string(),
        }
    }
    pub fn as_clip(&self) -> Option<InstanceId> {
        match self {
            Value::Clip(id) => Some(*id),
            _ => None,
        }
    }
    /// AVM1 boolean coercion (used by `Not`, `If`).
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty() && s != "0" && s != "false",
            Value::Undefined | Value::Null => false,
            Value::Clip(_) | Value::Function(_) | Value::Object(_) => true,
        }
    }
}

/// Holds all clip state. Owned by the renderer; passed in to `exec`.
#[derive(Default)]
pub struct AvmEngine {
    pub clips: HashMap<InstanceId, ClipState>,
}

impl AvmEngine {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn ensure(
        &mut self,
        id: InstanceId,
        parent: Option<InstanceId>,
        total_frames: u16,
    ) -> &mut ClipState {
        self.clips
            .entry(id)
            .or_insert_with(|| ClipState::new(total_frames, parent))
    }
}

/// Result of running a script — frame jumps need to be applied AFTER exec
/// returns so we don't fight with the interpreter's own state mutation.
/// `spawns` carries any clips the script asked to spawn at runtime
/// (`attachMovie` / `duplicateMovieClip`). The renderer processes them after
/// exec returns so it can do the SwfDoc-symbol lookup and create the new
/// instance with proper parentage.
#[derive(Debug, Default)]
pub struct ExecOutcome {
    pub stop: bool,
    pub play: bool,
    pub goto: Option<u16>,
    pub spawns: Vec<SpawnRequest>,
    /// Set by `Return` action — the value popped from the stack at the
    /// return site. The recursive caller (when invoking a user-defined
    /// `Function`) reads this and pushes it onto its own stack.
    pub return_value: Option<Value>,
}

/// A runtime clip-spawn request emitted by the AVM1 interpreter.
#[derive(Debug, Clone)]
pub enum SpawnRequest {
    /// `target.attachMovie(symbol_name, instance_name, depth, [init_obj])`
    /// — spawn a fresh instance of the named library symbol on `target` at
    /// `depth`. The optional `init_obj`'s key/value pairs are copied to the
    /// new clip's `vars` map (covers `_x`/`_y`/etc. set at spawn time).
    AttachMovie {
        target: InstanceId,
        symbol_name: String,
        instance_name: String,
        depth: i32,
        init_obj: Option<HashMap<String, Value>>,
    },
    /// `source.duplicateMovieClip(instance_name, depth)` — clone an existing
    /// clip at a new depth on its parent. The renderer resolves which
    /// character to place by looking at `source`'s current placement.
    Duplicate {
        source: InstanceId,
        instance_name: String,
        depth: i32,
    },
}

/// Execute a chunk of AVM1 bytecode against `this_id`. `swf_version` should be
/// the file's reported version (`SwfDoc::stage_size` is per-file; we pass it
/// down).
pub fn exec(
    bytecode: &[u8],
    swf_version: u8,
    this_id: InstanceId,
    engine: &mut AvmEngine,
) -> ExecOutcome {
    let mut stack: Vec<Value> = Vec::with_capacity(8);
    let mut constant_pool: Vec<String> = Vec::new();
    let mut outcome = ExecOutcome::default();
    let mut reader = Reader::new(bytecode, swf_version);
    // 256-slot register file. AVM1 `StoreRegister` writes the top-of-stack
    // here, used by the AS2 compiler as scratch storage. AVM1 (SWF 5+) has
    // 4 base registers; DefineFunction2 can declare more (we don't model
    // function frames yet, so 256 is generous).
    let mut registers: Vec<Value> = vec![Value::Undefined; 256];

    loop {
        let action = match reader.read_action() {
            Ok(a) => a,
            Err(_) => break,
        };
        match action {
            Action::End => break,
            Action::ConstantPool(cp) => {
                constant_pool = cp
                    .strings
                    .iter()
                    .map(|s| s.to_string_lossy(swf::UTF_8).to_string())
                    .collect();
            }
            Action::Push(Push { values }) => {
                for v in values {
                    let pushed = match &v {
                        PushValue::Register(idx) => registers
                            .get(*idx as usize)
                            .cloned()
                            .unwrap_or(Value::Undefined),
                        _ => push_value_to_value(&v, &constant_pool),
                    };
                    stack.push(pushed);
                }
            }
            Action::Pop => {
                stack.pop();
            }
            Action::Stop => {
                outcome.stop = true;
                if let Some(s) = engine.clips.get_mut(&this_id) {
                    s.playing = false;
                }
            }
            Action::Play => {
                outcome.play = true;
                if let Some(s) = engine.clips.get_mut(&this_id) {
                    s.playing = true;
                }
            }
            Action::RandomNumber => {
                let n = pop_number(&mut stack).max(0.0);
                let r = if n <= 0.0 {
                    0.0
                } else {
                    (rand_u32() as f64 / u32::MAX as f64 * n).floor()
                };
                stack.push(Value::Number(r));
            }
            Action::Add2 => {
                // AVM1's `+` adds numbers OR concatenates strings if either
                // operand is a string. Spell 802 only uses it numerically,
                // but model the type check faithfully so other spells don't
                // silently NaN.
                let b = stack.pop().unwrap_or(Value::Undefined);
                let a = stack.pop().unwrap_or(Value::Undefined);
                let result = if matches!(a, Value::String(_)) || matches!(b, Value::String(_)) {
                    Value::String(format!("{}{}", a.as_string(), b.as_string()))
                } else {
                    Value::Number(a.as_f64() + b.as_f64())
                };
                stack.push(result);
            }
            Action::Subtract => {
                let b = pop_number(&mut stack);
                let a = pop_number(&mut stack);
                stack.push(Value::Number(a - b));
            }
            Action::Multiply => {
                let b = pop_number(&mut stack);
                let a = pop_number(&mut stack);
                stack.push(Value::Number(a * b));
            }
            Action::Divide => {
                let b = pop_number(&mut stack);
                let a = pop_number(&mut stack);
                stack.push(Value::Number(a / b));
            }
            Action::GetVariable => {
                let name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let v = resolve_get(&name, this_id, engine);
                stack.push(v);
            }
            Action::SetVariable => {
                let value = stack.pop().unwrap_or(Value::Undefined);
                let name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                resolve_set(&name, value, this_id, engine);
            }
            Action::GetProperty => {
                let prop_idx = pop_number(&mut stack) as i32;
                let target_path = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let target_id = resolve_target(&target_path, this_id, engine);
                let value = match target_id.and_then(|id| engine.clips.get(&id)) {
                    Some(state) => property_get(state, prop_idx),
                    None => Value::Undefined,
                };
                stack.push(value);
            }
            Action::SetProperty => {
                let value = stack.pop().unwrap_or(Value::Undefined);
                let prop_idx = pop_number(&mut stack) as i32;
                let target_path = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                if let Some(target_id) = resolve_target(&target_path, this_id, engine)
                    && let Some(state) = engine.clips.get_mut(&target_id)
                {
                    property_set(state, prop_idx, value);
                }
            }
            Action::GetMember => {
                let member = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let target = stack.pop().unwrap_or(Value::Undefined);
                let value = match target {
                    Value::Clip(id) => match engine.clips.get(&id) {
                        Some(state) => match prop_index(&member) {
                            Some(idx) => property_get(state, idx),
                            None => state.vars.get(&member).cloned().unwrap_or(Value::Undefined),
                        },
                        None => Value::Undefined,
                    },
                    Value::Object(o) => o.borrow().get(&member).cloned().unwrap_or(Value::Undefined),
                    _ => Value::Undefined,
                };
                stack.push(value);
            }
            Action::SetMember => {
                let value = stack.pop().unwrap_or(Value::Undefined);
                let member = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let target = stack.pop().unwrap_or(Value::Undefined);
                match target {
                    Value::Clip(id) => {
                        if let Some(state) = engine.clips.get_mut(&id) {
                            match prop_index(&member) {
                                Some(idx) => property_set(state, idx, value),
                                None => {
                                    state.vars.insert(member, value);
                                }
                            }
                        }
                    }
                    Value::Object(o) => {
                        o.borrow_mut().insert(member, value);
                    }
                    _ => {}
                }
            }
            Action::GotoFrame2(g2) => {
                // Stack: [frame]. `set_playing` flag controls whether we
                // play or stop after the jump.
                let frame_value = stack.pop().unwrap_or(Value::Undefined);
                let frame = match &frame_value {
                    Value::Number(n) => *n as i64,
                    Value::String(s) => s.parse::<i64>().unwrap_or(0),
                    _ => 0,
                };
                // GotoFrame2 frame numbers are 0-indexed plus the
                // `scene_offset`. Most Dofus content uses 0 scene offset and
                // 0-indexed frames, but the AS source `gotoAndStop(N+1)` we
                // dump is the human form — bytecode pushed N then the goto
                // adds 1 internally. Treat the popped value as the 1-indexed
                // frame already.
                // Flash CLAMPS goto target to [1, total_frames]. spell 802's
                // sp6.d3 mask handler does `gotoAndStop(random(2)+1)` which
                // can pick frame 2 on a 1-frame sprite — Flash silently
                // clamps to 1. Without this, our `_currentframe` reads back
                // 2 forever, diverging from Flash logs.
                let raw = (frame as i64 + i64::from(g2.scene_offset)).max(1) as u16;
                if let Some(state) = engine.clips.get_mut(&this_id) {
                    let target_frame = raw.min(state.total_frames.max(1));
                    outcome.goto = Some(target_frame);
                    state.current_frame = target_frame;
                    state.playing = g2.set_playing;
                } else {
                    outcome.goto = Some(raw);
                }
                if g2.set_playing {
                    outcome.play = true;
                } else {
                    outcome.stop = true;
                }
            }
            Action::CallMethod => {
                // Stack: [method_name, target, num_args, args...]
                let method_name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let target = stack.pop().unwrap_or(Value::Undefined);
                let num_args = pop_number(&mut stack) as usize;
                let mut args = Vec::with_capacity(num_args);
                for _ in 0..num_args {
                    args.push(stack.pop().unwrap_or(Value::Undefined));
                }
                // Resolve member: if it's a user-defined Function, recurse
                // exec into it. `this` for the call = the resolved target.
                let resolved_member = lookup_member(&target, &method_name, engine);
                let result = if let Value::Function(fn_def) = resolved_member {
                    let call_this = match &target {
                        Value::Clip(id) => *id,
                        _ => this_id,
                    };
                    let sub = exec(&fn_def.code, fn_def.swf_version, call_this, engine);
                    outcome.spawns.extend(sub.spawns);
                    sub.return_value.unwrap_or(Value::Undefined)
                } else {
                    call_method(
                        &method_name,
                        &target,
                        &args,
                        this_id,
                        engine,
                        &mut outcome,
                    )
                };
                stack.push(result);
            }
            Action::CallFunction => {
                // Stack: [function_name, num_args, args...]
                let fn_name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let num_args = pop_number(&mut stack) as usize;
                let mut args = Vec::with_capacity(num_args);
                for _ in 0..num_args {
                    args.push(stack.pop().unwrap_or(Value::Undefined));
                }
                // Resolve as a local user-defined function first; if it's
                // a Function, invoke it (the AS2 source `myFn(x)` ends up
                // here when `myFn` is a clip-local or root-level function).
                let resolved = resolve_get(&fn_name, this_id, engine);
                let result = if let Value::Function(fn_def) = resolved {
                    let sub = exec(&fn_def.code, fn_def.swf_version, this_id, engine);
                    outcome.spawns.extend(sub.spawns);
                    sub.return_value.unwrap_or(Value::Undefined)
                } else {
                    call_global(&fn_name, &args, this_id, engine, &mut outcome)
                };
                stack.push(result);
            }
            // Trace: pops one value, logs it to stderr. Used for parity
            // checking against Flash Player Debugger's flashlog.txt — when
            // we feed a traced SWF through our renderer, the trace output
            // can be diffed line-by-line against Flash's.
            Action::Trace => {
                let v = stack.pop().unwrap_or(Value::Undefined);
                #[cfg(not(target_arch = "wasm32"))]
                eprintln!("{}", v.as_string());
            }
            // ---- Comparisons (32 spells use Less2, 30 Greater, 26 Equals2) ----
            Action::Less2 => {
                let b = pop_number(&mut stack);
                let a = pop_number(&mut stack);
                stack.push(Value::Bool(a < b));
            }
            Action::Greater => {
                let b = pop_number(&mut stack);
                let a = pop_number(&mut stack);
                stack.push(Value::Bool(a > b));
            }
            Action::Equals2 => {
                // Loose equality with type coercion (==). Strings compare
                // as strings; numbers as numbers; null/undefined are equal
                // to each other.
                let b = stack.pop().unwrap_or(Value::Undefined);
                let a = stack.pop().unwrap_or(Value::Undefined);
                let eq = match (&a, &b) {
                    (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
                    (Value::String(_), _) | (_, Value::String(_)) => a.as_string() == b.as_string(),
                    _ => a.as_f64() == b.as_f64(),
                };
                stack.push(Value::Bool(eq));
            }
            // ---- Boolean (49 spells use Not) ----
            Action::Not => {
                let v = stack.pop().unwrap_or(Value::Undefined);
                stack.push(Value::Bool(!v.as_bool()));
            }
            // ---- Arithmetic (32 Increment, 5 Modulo, 15 BitAnd) ----
            Action::Increment => {
                let n = pop_number(&mut stack);
                stack.push(Value::Number(n + 1.0));
            }
            Action::Decrement => {
                let n = pop_number(&mut stack);
                stack.push(Value::Number(n - 1.0));
            }
            Action::Modulo => {
                let b = pop_number(&mut stack);
                let a = pop_number(&mut stack);
                stack.push(Value::Number(a % b));
            }
            Action::BitAnd => {
                let b = pop_number(&mut stack) as i32;
                let a = pop_number(&mut stack) as i32;
                stack.push(Value::Number(f64::from(a & b)));
            }
            Action::BitOr => {
                let b = pop_number(&mut stack) as i32;
                let a = pop_number(&mut stack) as i32;
                stack.push(Value::Number(f64::from(a | b)));
            }
            Action::BitXor => {
                let b = pop_number(&mut stack) as i32;
                let a = pop_number(&mut stack) as i32;
                stack.push(Value::Number(f64::from(a ^ b)));
            }
            Action::BitLShift => {
                let b = pop_number(&mut stack) as i32;
                let a = pop_number(&mut stack) as i32;
                stack.push(Value::Number(f64::from(a << (b & 0x1f))));
            }
            Action::BitRShift => {
                let b = pop_number(&mut stack) as i32;
                let a = pop_number(&mut stack) as i32;
                stack.push(Value::Number(f64::from(a >> (b & 0x1f))));
            }
            // ---- Control flow (49 If, 14 Jump) ----
            Action::Jump(j) => {
                // Jump's offset is signed: positive jumps forward, negative
                // jumps back. Reader::seek modifies the input slice within
                // the original bytecode — exactly what we need here.
                reader.seek(bytecode, j.offset);
            }
            Action::If(j) => {
                let cond = stack.pop().unwrap_or(Value::Undefined).as_bool();
                if cond {
                    reader.seek(bytecode, j.offset);
                }
            }
            // ---- Register file (27 spells) ----
            // StoreRegister copies the top of the stack into a register
            // WITHOUT popping. The AS2 compiler emits this for repeated
            // expressions (`r = expensive_call(); use r; use r;`).
            Action::StoreRegister(r) => {
                if let Some(v) = stack.last() {
                    let idx = r.register as usize;
                    if idx < registers.len() {
                        registers[idx] = v.clone();
                    }
                }
            }
            // ---- Variable scoping (4 spells) ----
            // DefineLocal pops [name, value]. We don't model true function
            // scopes yet, so locals land on the same `vars` map as
            // SetVariable — fine in practice because spell scripts don't
            // recurse function calls.
            Action::DefineLocal => {
                let value = stack.pop().unwrap_or(Value::Undefined);
                let name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                resolve_set(&name, value, this_id, engine);
            }
            Action::DefineLocal2 => {
                // Same as DefineLocal but pops only the name; sets it to
                // undefined.
                let name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                resolve_set(&name, Value::Undefined, this_id, engine);
            }
            // ---- Old-style GotoFrame (8 spells) ----
            // Frame field is 0-indexed in the SWF spec; we store
            // 1-indexed. After goto, playing semantics depend on whether
            // the prior tag was a Stop/Play — we just set the frame; the
            // outer renderer handles the rest.
            Action::GotoFrame(g) => {
                let target = (u32::from(g.frame) + 1) as u16; // 0-based → 1-based
                if let Some(state) = engine.clips.get_mut(&this_id) {
                    let target = target.max(1).min(state.total_frames.max(1));
                    state.current_frame = target;
                    outcome.goto = Some(target);
                }
            }
            // RemoveSprite: pops target, removes that clip.
            Action::RemoveSprite => {
                let target_path = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                if let Some(target_id) = resolve_target(&target_path, this_id, engine)
                    && let Some(state) = engine.clips.get_mut(&target_id)
                {
                    state.removed = true;
                }
            }
            // ---- DefineFunction (3 spells) ----
            // Pushes a Function value onto the stack. The function body's
            // bytecode is captured verbatim so we can re-execute it on
            // every CallMethod / event dispatch.
            Action::DefineFunction(df) => {
                let params: Vec<String> = df
                    .params
                    .iter()
                    .map(|p| p.to_string_lossy(swf::UTF_8).to_string())
                    .collect();
                let fn_def = Rc::new(FnDef {
                    params,
                    code: df.actions.to_vec(),
                    swf_version,
                    register_count: 0,
                });
                let name = df.name.to_string_lossy(swf::UTF_8).to_string();
                let value = Value::Function(fn_def);
                if name.is_empty() {
                    // Anonymous function literal — push for the next op
                    // (typically SetMember or SetVariable) to grab.
                    stack.push(value);
                } else {
                    // Named function — registered as a local variable
                    // AND pushed (matches Flash behavior).
                    resolve_set(&name, value.clone(), this_id, engine);
                    stack.push(value);
                }
            }
            Action::DefineFunction2(df) => {
                let params: Vec<String> = df
                    .params
                    .iter()
                    .map(|p| p.name.to_string_lossy(swf::UTF_8).to_string())
                    .collect();
                let fn_def = Rc::new(FnDef {
                    params,
                    code: df.actions.to_vec(),
                    swf_version,
                    register_count: df.register_count,
                });
                let name = df.name.to_string_lossy(swf::UTF_8).to_string();
                let value = Value::Function(fn_def);
                if name.is_empty() {
                    stack.push(value);
                } else {
                    resolve_set(&name, value.clone(), this_id, engine);
                    stack.push(value);
                }
            }
            // ---- Return (1 spell directly; many via DefineFunction bodies) ----
            Action::Return => {
                outcome.return_value = stack.pop();
                break;
            }
            // ---- InitObject (1 spell directly + many DefineFunction
            // bodies) — pops num_pairs, then num_pairs * 2 items (key,
            // value pairs in interleaved order on the stack), pushes a
            // fresh Object value.
            Action::InitObject => {
                let num_pairs = pop_number(&mut stack) as usize;
                let mut map: HashMap<String, Value> = HashMap::with_capacity(num_pairs);
                for _ in 0..num_pairs {
                    let value = stack.pop().unwrap_or(Value::Undefined);
                    let key = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                    map.insert(key, value);
                }
                stack.push(Value::Object(Rc::new(RefCell::new(map))));
            }
            // NewObject: pops object_class_name, num_args, args... — used
            // for `new Object()` / `new MyClass(args)`. We don't model
            // classes, but a bare `new Object()` should yield an empty
            // object literal.
            Action::NewObject => {
                let _class_name = stack.pop().map(|v| v.as_string()).unwrap_or_default();
                let num_args = pop_number(&mut stack) as usize;
                for _ in 0..num_args {
                    stack.pop();
                }
                stack.push(Value::Object(Rc::new(RefCell::new(HashMap::new()))));
            }
            // Quietly ignore opcodes we don't model — this lets the script
            // run as far as possible. Most spells degrade to "default
            // timeline" behavior rather than crashing.
            _ => {
                // Log every distinct unimplemented action variant so the
                // coverage survey across all spells gets the full set, not
                // just whichever one fires first.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    use std::sync::Mutex;
                    static SEEN: Mutex<Option<std::collections::HashSet<String>>> =
                        Mutex::new(None);
                    let key = format!("{action:?}");
                    let head: String = key
                        .split(|c: char| c == '(' || c == ' ')
                        .next()
                        .unwrap_or(&key)
                        .to_string();
                    let mut guard = SEEN.lock().unwrap();
                    let set = guard.get_or_insert_with(Default::default);
                    if set.insert(head.clone()) {
                        eprintln!("[avm1] unimplemented action: {head}");
                    }
                }
            }
        }
    }
    outcome
}

fn push_value_to_value(v: &PushValue<'_>, pool: &[String]) -> Value {
    match v {
        PushValue::Undefined => Value::Undefined,
        PushValue::Null => Value::Null,
        PushValue::Bool(b) => Value::Bool(*b),
        PushValue::Int(i) => Value::Number(f64::from(*i)),
        PushValue::Float(f) => Value::Number(f64::from(*f)),
        PushValue::Double(d) => Value::Number(*d),
        PushValue::Str(s) => Value::String(s.to_string_lossy(swf::UTF_8).to_string()),
        PushValue::Register(_) => Value::Undefined,
        PushValue::ConstantPool(idx) => pool
            .get(*idx as usize)
            .cloned()
            .map(Value::String)
            .unwrap_or(Value::Undefined),
    }
}

fn pop_number(stack: &mut Vec<Value>) -> f64 {
    stack.pop().map(|v| v.as_f64()).unwrap_or(f64::NAN)
}

/// Public re-export so render_avm1's `apply_spawns` can resolve init-object
/// keys (`_x`/`_y`/etc.) the same way the interpreter does.
pub fn prop_index_pub(name: &str) -> Option<i32> {
    prop_index(name)
}

/// Public re-export of `property_set` for the same reason.
pub fn property_set_pub(state: &mut ClipState, idx: i32, value: Value) {
    property_set(state, idx, value)
}

/// Map a Flash `_<name>` property accessor or numeric SetProperty index to a
/// canonical numeric index. Returns None for non-property names.
fn prop_index(name: &str) -> Option<i32> {
    Some(match name {
        "_x" => 0,
        "_y" => 1,
        "_xscale" => 2,
        "_yscale" => 3,
        "_currentframe" => 4,
        "_totalframes" => 5,
        "_alpha" => 6,
        "_visible" => 7,
        "_width" => 8,
        "_height" => 9,
        "_rotation" => 10,
        _ => return None,
    })
}

fn property_get(state: &ClipState, idx: i32) -> Value {
    match idx {
        0 => Value::Number(state.x),
        1 => Value::Number(state.y),
        2 => Value::Number(state.xscale),
        3 => Value::Number(state.yscale),
        4 => Value::Number(f64::from(state.current_frame)),
        5 => Value::Number(f64::from(state.total_frames)),
        6 => Value::Number(state.alpha),
        10 => Value::Number(state.rotation),
        _ => Value::Number(0.0),
    }
}

fn property_set(state: &mut ClipState, idx: i32, value: Value) {
    // Flash silently coerces NaN/Inf assignments to 0 via i32 cast in the
    // twips path. We mirror that here so `_x = Math.sin(undef) * 100`
    // doesn't poison state.x with NaN, which would later flow into the
    // world matrix and make Vello produce a fully transparent frame
    // (silent failure mode in render-to-texture).
    let mut n = value.as_f64();
    if !n.is_finite() {
        n = 0.0;
    }
    // Setting any transform property flips the `transformed_by_script` flag.
    // Subsequent timeline Modify/Place matrix updates on this clip become
    // no-ops (Flash semantic — see `ClipState::transformed_by_script`).
    // _alpha (6) does NOT count — color_transform isn't strictly part of
    // the matrix. But Ruffle's apply_place_object also gates color_transform
    // on this flag, so we include 6 too for parity.
    match idx {
        0 | 1 | 2 | 3 | 6 | 10 => state.transformed_by_script = true,
        _ => {}
    }
    match idx {
        0 => state.x = n,
        1 => state.y = n,
        2 => state.xscale = n,
        3 => state.yscale = n,
        6 => state.alpha = n,
        10 => state.rotation = n,
        _ => {} // Other properties (_visible, _width, _height) — not modelled.
    }
}

/// Resolve a target path string like `""`, `"this"`, `"_parent"`, `"_root"`.
/// Returns the InstanceId of the resolved clip, or None if the path didn't
/// resolve (we don't support deep absolute paths yet).
fn resolve_target(
    path: &str,
    this_id: InstanceId,
    engine: &AvmEngine,
) -> Option<InstanceId> {
    match path {
        "" | "this" => Some(this_id),
        "_parent" => engine.clips.get(&this_id).and_then(|s| s.parent),
        "_root" => {
            // Walk up to the topmost ancestor.
            let mut cur = this_id;
            while let Some(parent) = engine.clips.get(&cur).and_then(|s| s.parent) {
                cur = parent;
            }
            Some(cur)
        }
        _ => None,
    }
}

/// Look up `member` on `target`. Walks Object property bags and Clip
/// `vars`/property indices the same way `GetMember` does. Used by
/// `CallMethod` to detect user-defined Function values bound to clip
/// instances (e.g. `this.onEnterFrame = function(){…}` then later invoked
/// indirectly).
fn lookup_member(target: &Value, member: &str, engine: &AvmEngine) -> Value {
    match target {
        Value::Clip(id) => match engine.clips.get(id) {
            Some(state) => match prop_index(member) {
                Some(idx) => property_get(state, idx),
                None => state.vars.get(member).cloned().unwrap_or(Value::Undefined),
            },
            None => Value::Undefined,
        },
        Value::Object(o) => o.borrow().get(member).cloned().unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    }
}

fn resolve_get(name: &str, this_id: InstanceId, engine: &AvmEngine) -> Value {
    // Special pseudo-vars that aren't local: `_parent` and `_root` resolve to
    // a clip reference. Anything else falls through to the local var dict.
    match name {
        "_parent" => engine
            .clips
            .get(&this_id)
            .and_then(|s| s.parent)
            .map(Value::Clip)
            .unwrap_or(Value::Undefined),
        "_root" => {
            let mut cur = this_id;
            while let Some(parent) = engine.clips.get(&cur).and_then(|s| s.parent) {
                cur = parent;
            }
            Value::Clip(cur)
        }
        _ => engine
            .clips
            .get(&this_id)
            .and_then(|s| s.vars.get(name).cloned())
            .unwrap_or(Value::Undefined),
    }
}

fn resolve_set(name: &str, value: Value, this_id: InstanceId, engine: &mut AvmEngine) {
    if let Some(state) = engine.clips.get_mut(&this_id) {
        state.vars.insert(name.to_string(), value);
    }
}

fn call_method(
    name: &str,
    target: &Value,
    args: &[Value],
    this_id: InstanceId,
    engine: &mut AvmEngine,
    outcome: &mut ExecOutcome,
) -> Value {
    // Math methods. AS2 calls them as `Math.sin(x)` which compiles to a
    // CallMethod with target=lookupGetVariable("Math") — and we don't
    // register a Math object, so the lookup returns Undefined. Match by
    // method name regardless of target so the math actually runs.
    // Without this, `_x = Math.sin(i) * 100` evaluates to NaN, the matrix
    // translation becomes NaN, and Vello produces (0,0,0,0) for the entire
    // frame (silent failure mode in render-to-texture).
    if let Some(v) = call_math_method(name, args) {
        return v;
    }
    let target_id = match target {
        Value::Clip(id) => *id,
        _ => this_id,
    };
    match name {
        "removeMovieClip" => {
            if let Some(state) = engine.clips.get_mut(&target_id) {
                state.removed = true;
            }
            Value::Undefined
        }
        "stop" => {
            if let Some(state) = engine.clips.get_mut(&target_id) {
                state.playing = false;
            }
            Value::Undefined
        }
        "play" => {
            if let Some(state) = engine.clips.get_mut(&target_id) {
                state.playing = true;
            }
            Value::Undefined
        }
        "gotoAndStop" => {
            if let Some(state) = engine.clips.get_mut(&target_id) {
                let frame = args.first().map(|v| v.as_f64()).unwrap_or(1.0).max(1.0) as u16;
                state.current_frame = frame;
                state.playing = false;
            }
            Value::Undefined
        }
        "gotoAndPlay" => {
            if let Some(state) = engine.clips.get_mut(&target_id) {
                let frame = args.first().map(|v| v.as_f64()).unwrap_or(1.0).max(1.0) as u16;
                state.current_frame = frame;
                state.playing = true;
            }
            Value::Undefined
        }
        "attachMovie" => {
            // Args (in code order): linkage_name, instance_name, depth, [init_obj].
            // The renderer creates the clip after exec returns; we just record
            // the request here. The Value::Clip we return is a *placeholder* —
            // it points at the parent so subsequent property access doesn't
            // crash. The renderer fixes up references on the next tick.
            let linkage = args.first().map(|v| v.as_string()).unwrap_or_default();
            let inst_name = args.get(1).map(|v| v.as_string()).unwrap_or_default();
            let depth = args.get(2).map(|v| v.as_f64() as i32).unwrap_or(0);
            let init_obj = args.get(3).and_then(|v| match v {
                Value::Object(o) => Some(o.borrow().clone()),
                _ => None,
            });
            outcome.spawns.push(SpawnRequest::AttachMovie {
                target: target_id,
                symbol_name: linkage,
                instance_name: inst_name,
                depth,
                init_obj,
            });
            Value::Clip(target_id)
        }
        "duplicateMovieClip" => {
            // Args: instance_name, depth.
            let inst_name = args.first().map(|v| v.as_string()).unwrap_or_default();
            let depth = args.get(1).map(|v| v.as_f64() as i32).unwrap_or(0);
            outcome.spawns.push(SpawnRequest::Duplicate {
                source: target_id,
                instance_name: inst_name,
                depth,
            });
            Value::Clip(target_id)
        }
        _ => Value::Undefined,
    }
}

/// AS2 Math.* helpers. Returns Some(result) if `name` is a known Math
/// method; None otherwise. Argument count matches AS2: each method pulls
/// its arity from `args` and defaults missing ones to NaN.
fn call_math_method(name: &str, args: &[Value]) -> Option<Value> {
    let a0 = || args.first().map(|v| v.as_f64()).unwrap_or(f64::NAN);
    let a1 = || args.get(1).map(|v| v.as_f64()).unwrap_or(f64::NAN);
    let v = match name {
        "sin" => a0().sin(),
        "cos" => a0().cos(),
        "tan" => a0().tan(),
        "asin" => a0().asin(),
        "acos" => a0().acos(),
        "atan" => a0().atan(),
        "atan2" => a0().atan2(a1()),
        "sqrt" => a0().sqrt(),
        "abs" => a0().abs(),
        "floor" => a0().floor(),
        "ceil" => a0().ceil(),
        "round" => a0().round(),
        "exp" => a0().exp(),
        "log" => a0().ln(),
        "pow" => a0().powf(a1()),
        "min" => a0().min(a1()),
        "max" => a0().max(a1()),
        "random" => rand_u32() as f64 / u32::MAX as f64,
        _ => return None,
    };
    Some(Value::Number(v))
}

fn call_global(
    name: &str,
    args: &[Value],
    this_id: InstanceId,
    engine: &mut AvmEngine,
    outcome: &mut ExecOutcome,
) -> Value {
    match name {
        "random" => {
            let n = args.first().map(|v| v.as_f64()).unwrap_or(0.0).max(0.0);
            if n <= 0.0 {
                Value::Number(0.0)
            } else {
                let r = (rand_u32() as f64 / u32::MAX as f64 * n).floor();
                Value::Number(r)
            }
        }
        // `attachMovie` / `duplicateMovieClip` are also reachable as bare
        // function calls in some compiled output, so route them through here
        // too. Target = current clip (this).
        "attachMovie" => {
            let linkage = args.first().map(|v| v.as_string()).unwrap_or_default();
            let inst_name = args.get(1).map(|v| v.as_string()).unwrap_or_default();
            let depth = args.get(2).map(|v| v.as_f64() as i32).unwrap_or(0);
            let init_obj = args.get(3).and_then(|v| match v {
                Value::Object(o) => Some(o.borrow().clone()),
                _ => None,
            });
            outcome.spawns.push(SpawnRequest::AttachMovie {
                target: this_id,
                symbol_name: linkage,
                instance_name: inst_name,
                depth,
                init_obj,
            });
            Value::Clip(this_id)
        }
        _ => {
            let _ = engine;
            Value::Undefined
        }
    }
}

/// Cheap PRNG. We don't need cryptographic randomness — Dofus spells just want
/// flicker and per-instance jitter — and pulling in `rand` would inflate the
/// WASM bundle. xorshift32 is good enough.
fn rand_u32() -> u32 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u32> = const { Cell::new(0x1234_5678) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = 0x1234_5678;
        }
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        s.set(x);
        x
    })
}
