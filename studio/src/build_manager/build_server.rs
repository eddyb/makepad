use {
    crate::{
        makepad_code_editor::text::{Position, Length},
        makepad_micro_serde::*,
        build_manager::{
            build_protocol::*,
            child_process::{
                ChildStdIn,
                ChildProcess,
                ChildStdIO
            },
            rustc_json::*,
        },
    },
    std::{
        collections::HashMap,
        fmt,
        path::PathBuf,
        sync::{Arc, RwLock, Mutex, mpsc::Sender},
    },
};

struct BuildServerProcess {
    cmd_id: BuildCmdId,
    stdin_sender: Mutex<Sender<ChildStdIn> >,
    line_sender: Mutex<Sender<ChildStdIO> >,
}

struct BuildServerShared {
    path: PathBuf,
    // here we should store our connections send slots
    processes: HashMap<BuildProcess, BuildServerProcess>
}

pub struct BuildServer {
    shared: Arc<RwLock<BuildServerShared >>,
}

impl BuildServer {
    pub fn new<P: Into<PathBuf >> (path: P) -> BuildServer {
        BuildServer {
            shared: Arc::new(RwLock::new(BuildServerShared {
                path: path.into(),
                processes: Default::default()
            })),
        }
    }
    
    pub fn connect(&mut self, msg_sender: Box<dyn MsgSender>) -> BuildConnection {
        BuildConnection {
            shared: self.shared.clone(),
            msg_sender,
        }
    }
}

pub struct BuildConnection {
    //    connection_id: ConnectionId,
    shared: Arc<RwLock<BuildServerShared >>,
    msg_sender: Box<dyn MsgSender>,
}

#[derive(Debug, PartialEq)]
enum StdErrState {
    First,
    Sync,
    Desync,
    Running,
}


impl BuildConnection {
    
    pub fn stop(&self, cmd_id: BuildCmdId) {
        let shared = self.shared.clone();
        
        let shared = shared.write().unwrap();
        if let Some(proc) = shared.processes.values().find(|v| v.cmd_id == cmd_id) {
            let line_sender = proc.line_sender.lock().unwrap();
            let _ = line_sender.send(ChildStdIO::Kill);
        }
    }
    
    pub fn run(&self, what: BuildProcess, cmd_id: BuildCmdId, http:String) {
        
        let shared = self.shared.clone();
        let msg_sender = self.msg_sender.clone();
        // alright lets run a cargo check and parse its output
        let path = shared.read().unwrap().path.clone();
        
        let args: Vec<String> = match &what.target {
            #[cfg(not(target_os="windows"))]
            BuildTarget::ReleaseStudio => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "run".into(),
                "-p".into(),
                what.binary.clone(),
                "--message-format=json".into(),
                "--release".into(),
                "--".into(),
                "--message-format=json".into(),
                "--stdin-loop".into(),
            ],
            #[cfg(not(target_os="windows"))]
            BuildTarget::DebugStudio => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "run".into(),
                "-p".into(),
                what.binary.clone(),
                "--message-format=json".into(),
                "--".into(),
                "--message-format=json".into(),
                "--stdin-loop".into(),
            ],
            BuildTarget::Release => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "run".into(),
                "-p".into(),
                what.binary.clone(),
                "--message-format=json".into(),
                "--release".into(),
                "--".into(),
                "--message-format=json".into(),
            ],
            BuildTarget::Debug => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "run".into(),
                "-p".into(),
                what.binary.clone(),
                "--message-format=json".into(),
                "--".into(),
                "--message-format=json".into(),
            ],
            BuildTarget::Profiler => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "instruments".into(),
                "-t".into(),
                "time".into(),
                "-p".into(),
                what.binary.clone(),
                "--release".into(),
                "--message-format=json".into(),
                "--".into(),
                "--message-format=json".into(),
            ],
            BuildTarget::IosSim {org, app} => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "makepad".into(),
                "ios".into(),
                format!("--org={org}"),
                format!("--app={app}"),
                "run-sim".into(),
                "-p".into(),
                what.binary.clone(),
                "--release".into(),
                "--message-format=json".into(),
            ],
            BuildTarget::IosDevice {org, app} => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "makepad".into(),
                "ios".into(),
                format!("--org={org}"),
                format!("--app={app}"),
                "run-device".into(),
                "-p".into(),
                what.binary.clone(),
                "--release".into(),
                "--message-format=json".into(),
            ],
            BuildTarget::Android => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "makepad".into(),
                "android".into(),
                "run".into(),
                "-p".into(),
                what.binary.clone(),
                "--release".into(),
                "--message-format=json".into(),
            ],
            BuildTarget::WebAssembly => vec![
                "run".into(),
                "nightly".into(),
                "cargo".into(),
                "makepad".into(),
                "wasm".into(),
                "build".into(),
                "-p".into(),
                what.binary.clone(),
                "--release".into(),
                "--message-format=json".into(),
            ]
        };
        
        let env = [
            ("MAKEPAD_STUDIO_HTTP", http.as_str()),
            ("MAKEPAD", "lines")
        ];
        let process = ChildProcess::start("rustup", &args, path, &env).expect("Cannot start process");
        
        shared.write().unwrap().processes.insert(
            what,
            BuildServerProcess {
                cmd_id,
                stdin_sender: Mutex::new(process.stdin_sender.clone()),
                line_sender: Mutex::new(process.line_sender.clone())
            }
        );
        let mut stderr_state = StdErrState::First;
        let stdin_sender = process.stdin_sender.clone();
        std::thread::spawn(move || {
            // lets create a BuildProcess and run it
            while let Ok(line) = process.line_receiver.recv() {
                
                match line {
                    ChildStdIO::StdOut(line) => {
                        let comp_msg: Result<RustcCompilerMessage, DeJsonErr> = DeJson::deserialize_json(&line);
                        match comp_msg {
                            Ok(msg) => {
                                // alright we have a couple of 'reasons'
                                match msg.reason.as_str() {
                                    "makepad-signal" => {
                                        let _ = stdin_sender.send(ChildStdIn::Send(format!("{{\"Signal\":[{}]}}\n", msg.signal.unwrap())));
                                    }
                                    "makepad-error-log" | "compiler-message" => {
                                        msg_sender.process_compiler_message(cmd_id, msg);
                                    }
                                    "build-finished" => {
                                        if Some(true) == msg.success {
                                        }
                                        else {
                                        }
                                    }
                                    "compiler-artifact" => {
                                    }
                                    _ => ()
                                }
                            }
                            Err(_) => { // we should output a log string
                                //eprintln!("GOT ERROR {:?}", err);
                                msg_sender.send_stdin_to_host_msg(cmd_id, line);
                            }
                        }
                    }
                    ChildStdIO::StdErr(line) => {
                        // attempt to clean up stderr of cargo
                        match stderr_state {
                            StdErrState::First => {
                                if line.trim().starts_with("Compiling ") {
                                    msg_sender.send_bare_msg(cmd_id, LogItemLevel::Wait, line);
                                }
                                else if line.trim().starts_with("Finished ") {
                                    stderr_state = StdErrState::Running;
                                }
                                else if line.trim().starts_with("error: could not compile ") {
                                    msg_sender.send_bare_msg(cmd_id, LogItemLevel::Error, line);
                                }
                                else {
                                    stderr_state = StdErrState::Desync;
                                    msg_sender.send_bare_msg(cmd_id, LogItemLevel::Error, line);
                                }
                            }
                            StdErrState::Sync | StdErrState::Desync => {
                                msg_sender.send_bare_msg(cmd_id, LogItemLevel::Error, line);
                            }
                            StdErrState::Running => {
                                if line.trim().starts_with("Running ") {
                                    msg_sender.send_bare_msg(cmd_id, LogItemLevel::Wait, format!("{}", line.trim()));
                                    stderr_state = StdErrState::Sync
                                }
                                else {
                                    stderr_state = StdErrState::Desync;
                                    msg_sender.send_bare_msg(cmd_id, LogItemLevel::Error, line);
                                }
                            }
                        }
                    }
                    ChildStdIO::Term => {
                        msg_sender.send_bare_msg(cmd_id, LogItemLevel::Log, "process terminated".into());
                        break;
                    }
                    ChildStdIO::Kill => {
                        return process.kill();
                    }
                }
            };
        });
    }
    
    pub fn handle_cmd(&self, cmd_wrap: BuildCmdWrap) {
        match cmd_wrap.cmd {
            BuildCmd::Run(process, http) => {
                // lets kill all other 'whats'
                self.run(process, cmd_wrap.cmd_id, http);
            }
            BuildCmd::Stop => {
                // lets kill all other 'whats'
                self.stop(cmd_wrap.cmd_id);
            }
            BuildCmd::HostToStdin(msg) => {
                // ok lets fetch the running process from the cmd_id
                // and plug this msg on the standard input as serialiser json
                if let Ok(shared) = self.shared.read() {
                    for v in shared.processes.values() {
                        if v.cmd_id == cmd_wrap.cmd_id {
                            // lets send it on sender
                            if let Ok(stdin_sender) = v.stdin_sender.lock() {
                                let _ = stdin_sender.send(ChildStdIn::Send(msg));
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    
}

pub trait MsgSender: Send {
    fn box_clone(&self) -> Box<dyn MsgSender>;
    fn send_message(&self, wrap: LogItemWrap);
    
    fn send_bare_msg(&self, cmd_id: BuildCmdId, level: LogItemLevel, line: String) {
        let line = line.trim();
        self.send_message(
            cmd_id.wrap_msg(LogItem::Bare(LogItemBare {
                line: line.to_string(),
                level
            }))
        );
    }
    
    fn send_stdin_to_host_msg(&self, cmd_id: BuildCmdId, line: String) {
        self.send_message(
            cmd_id.wrap_msg(LogItem::StdinToHost(line))
        );
    }
    
    
    fn send_location_msg(&self, cmd_id: BuildCmdId, level: LogItemLevel, file_name: String, start: Position, length: Length, msg: String) {
        self.send_message(
            cmd_id.wrap_msg(LogItem::Location(LogItemLocation {
                level,
                file_name,
                start,
                length,
                msg
            }))
        );
    }
    
    fn process_compiler_message(&self, cmd_id: BuildCmdId, msg: RustcCompilerMessage) {
        if let Some(msg) = msg.message {
            
            let level = match msg.level.as_ref() {
                "error" => LogItemLevel::Error,
                "warning" => LogItemLevel::Warning,
                "log" => LogItemLevel::Log,
                "failure-note" => LogItemLevel::Error,
                "panic" => LogItemLevel::Panic,
                other => {
                    self.send_bare_msg(cmd_id, LogItemLevel::Error, format!("process_compiler_message: unexpected level {}", other));
                    return
                }
            };
            if let Some(span) = msg.spans.iter().find( | span | span.is_primary) {
                self.send_location_msg(cmd_id, level, span.file_name.clone(), span.start(), span.length(), msg.message.clone());
                /*
                if let Some(label) = &span.label {
                    self.send_location_msg(cmd_id, level, span.file_name.clone(), range, label.clone());
                }
                else if let Some(text) = span.text.iter().next() {
                    self.send_location_msg(cmd_id, level, span.file_name.clone(), range, text.text.clone());
                }
                else {
                    self.send_location_msg(cmd_id, level, span.file_name.clone(), range, msg.message.clone());
                }*/
            }
            else {
                if msg.message.trim().starts_with("aborting due to ") ||
                msg.message.trim().starts_with("For more information about this error") ||
                msg.message.trim().ends_with("warning emitted") ||
                msg.message.trim().ends_with("warnings emitted") {
                }
                else {
                    self.send_bare_msg(cmd_id, LogItemLevel::Warning, msg.message);
                }
            }
        }
    }
}

impl<F: Clone + Fn(LogItemWrap) + Send + 'static> MsgSender for F {
    fn box_clone(&self) -> Box<dyn MsgSender> {
        Box::new(self.clone())
    }
    
    fn send_message(&self, wrap: LogItemWrap) {
        self (wrap)
    }
}

impl Clone for Box<dyn MsgSender> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

impl fmt::Debug for dyn MsgSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MsgSender")
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ConnectionId(usize);
