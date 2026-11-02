//! Calls: live audio both ways with the user, and the speech that makes a call part of the chat.
//! A messenger only carries the audio; turning it into entries and the agent's text into speech
//! is the same for every messenger, so it lives here.
//! The user's turns, cut at pauses, are transcribed into the log, which wakes the agent;
//! the agent answers with `messagent speak`, which reaches the call through a socket.
//! The user's voice stops the agent's speaking, and the agent learns how much was heard.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::{Entry, Log, Party, watch::Waker};

/// The sample rate of a call's audio.
pub const RATE: u32 = 48_000;

/// The samples in one frame of a call's audio, 20 ms.
pub const FRAME: usize = RATE as usize / 50;

/// A call with the user, from its start to its end.
pub struct Call {
    /// The user's voice, as frames of mono PCM at `RATE`, silence included;
    /// it closes as the call ends.
    pub heard: mpsc::Receiver<Vec<i16>>,
    pub speaker: Box<dyn Speaker>,
}

/// What the agent says into the call.
pub trait Speaker: Send {
    /// Starts playing a WAV clip.
    fn play(&self, wav: Vec<u8>) -> Result<Box<dyn Clip>>;
}

/// A clip that plays.
pub trait Clip: Send {
    /// How far it has played, or nothing once it is over.
    fn position(&self) -> Option<Duration>;
    fn stop(&self);
}

/// How loud a frame must be to count as speech, as the root mean square of its samples.
const LOUD: f64 = 300.0;

/// How long a pause ends the user's turn.
const PAUSE: Duration = Duration::from_millis(800);

/// How much speech a turn needs, so that a cough or a click is no turn.
const SPEECH: Duration = Duration::from_millis(250);

const FRAME_TIME: Duration = Duration::from_millis(20);

/// The speech engine: one process for the call, one JSON line per request and per answer.
struct Engine {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Engine {
    fn start(command: &str) -> Result<Self> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("can't start the speech engine")?;
        let input = child.stdin.take().context("piped")?;
        let output = BufReader::new(child.stdout.take().context("piped")?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn request(&mut self, request: Value) -> Result<()> {
        writeln!(self.input, "{request}")?;
        self.input.flush()?;
        Ok(())
    }

    fn answer(&mut self) -> Result<Value> {
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            bail!("the speech engine ended");
        }
        let answer: Value = serde_json::from_str(&line)
            .with_context(|| format!("the speech engine answered no JSON: {line}"))?;
        if let Some(error) = answer.get("error") {
            bail!("the speech engine failed: {error}");
        }
        Ok(answer)
    }

    fn transcribe(&mut self, wav: &Path) -> Result<String> {
        self.request(json!({ "transcribe": wav }))?;
        let answer = self.answer()?;
        Ok(answer["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    /// Asks for speech and hands over each sentence's WAV file as it comes.
    fn speak(&mut self, text: &str, mut sentence: impl FnMut(PathBuf)) -> Result<()> {
        self.request(json!({ "speak": text }))?;
        loop {
            let answer = self.answer()?;
            if let Some(wav) = answer["wav"].as_str() {
                sentence(PathBuf::from(wav));
            }
            if answer["end"].as_bool() == Some(true) {
                return Ok(());
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// One thing the agent says, and how much of it has been played.
struct Saying {
    text: String,
    played: Duration,
    /// Whether it is over: played to its end, or cut off by the user.
    over: bool,
}

/// What a call shares between its threads.
struct Shared {
    log: Log,
    waker: Waker,
    /// The clips still to play, each with the index of its saying.
    queue: Mutex<Vec<(usize, PathBuf)>>,
    sayings: Mutex<Vec<Saying>>,
    /// The saying whose clip plays now.
    current: Mutex<Option<usize>>,
    /// Set while the user speaks over the agent, so that the player stops.
    interrupted: AtomicBool,
    ended: AtomicBool,
}

impl Shared {
    fn append(&self, entry: Entry) {
        if let Err(error) = self.log.append(&[entry]) {
            eprintln!("messagent: can't log: {error:#}");
        }
        self.waker.wake();
    }

    fn notice(&self, text: impl Into<String>) {
        self.append(Entry::notice(Timestamp::now(), text.into()));
    }
}

/// The calls of a messenger, one after the other, as long as `watch` runs.
/// What the agent says reaches the call through the socket.
pub fn serve(
    calls: mpsc::Receiver<Call>,
    engine: Option<String>,
    socket: PathBuf,
    log: Log,
    waker: Waker,
) -> Result<()> {
    let current: Arc<Mutex<Option<mpsc::Sender<String>>>> = Arc::default();
    fs::remove_file(&socket).ok();
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("can't listen on {}", socket.display()))?;
    let speaking = current.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Err(error) = take_request(stream, &speaking) {
                eprintln!("messagent: speak: {error:#}");
            }
        }
    });
    for call in calls {
        let Some(command) = &engine else {
            log.append(&[Entry::notice(
                Timestamp::now(),
                "The user started a call, but no speech engine is set up \
                 (`speech` in the config), so nothing is heard or said in it."
                    .into(),
            )])?;
            waker.wake();
            for _ in call.heard.iter() {}
            continue;
        };
        let (say, said) = mpsc::channel();
        *current.lock().expect("no panics") = Some(say);
        if let Err(error) = run(call, command, said, &log, &waker) {
            eprintln!("messagent: the call failed: {error:#}");
            log.append(&[Entry::notice(
                Timestamp::now(),
                format!("The call failed: {error:#}"),
            )])?;
            waker.wake();
        }
        *current.lock().expect("no panics") = None;
    }
    Ok(())
}

/// Takes one `messagent speak` request: the text, then an answer line, `ok` or the error.
fn take_request(
    mut stream: UnixStream,
    current: &Mutex<Option<mpsc::Sender<String>>>,
) -> Result<()> {
    let mut text = String::new();
    stream.read_to_string(&mut text)?;
    let answer = match current.lock().expect("no panics").as_ref() {
        Some(say) if say.send(text).is_ok() => "ok".to_string(),
        _ => "no call is on".to_string(),
    };
    writeln!(stream, "{answer}")?;
    Ok(())
}

/// Hands a text to the call to speak: what `messagent speak` does.
pub fn speak(socket: &Path, text: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket).context("no call is on")?;
    stream.write_all(text.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    match answer.trim() {
        "ok" => Ok(()),
        error => bail!("{error}"),
    }
}

fn run(
    call: Call,
    command: &str,
    said: mpsc::Receiver<String>,
    log: &Log,
    waker: &Waker,
) -> Result<()> {
    let engine = Arc::new(Mutex::new(Engine::start(command)?));
    let shared = Arc::new(Shared {
        log: Log {
            path: log.path.clone(),
            cursor: log.cursor.clone(),
        },
        waker: waker.clone(),
        queue: Mutex::default(),
        sayings: Mutex::default(),
        current: Mutex::default(),
        interrupted: AtomicBool::new(false),
        ended: AtomicBool::new(false),
    });
    shared.notice(
        "The user started a call. What they say arrives here as it is heard; \
         `messagent speak` answers aloud.",
    );
    let speaker = call.speaker;
    let player = {
        let shared = shared.clone();
        thread::spawn(move || play(&*speaker, &shared))
    };
    let talker = {
        let shared = shared.clone();
        let engine = engine.clone();
        thread::spawn(move || talk(said, &engine, &shared))
    };
    listen(call.heard, &engine, &shared);
    shared.ended.store(true, Ordering::SeqCst);
    player.join().ok();
    drop(talker);
    shared.notice("The call ended.");
    Ok(())
}

/// Cuts the user's voice into turns and logs each as it is transcribed.
fn listen(heard: mpsc::Receiver<Vec<i16>>, engine: &Mutex<Engine>, shared: &Shared) {
    let mut turn: Vec<i16> = Vec::new();
    let mut speech = Duration::ZERO;
    let mut quiet = Duration::ZERO;
    loop {
        // A messenger may send nothing while no one speaks, which is a pause all the same.
        let frame = match heard.recv_timeout(PAUSE / 4) {
            Ok(frame) => frame,
            Err(mpsc::RecvTimeoutError::Timeout) => Vec::new(),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let elapsed = if frame.is_empty() {
            PAUSE / 4
        } else {
            FRAME_TIME
        };
        let loud = rms(&frame) > LOUD;
        if loud {
            if speech == Duration::ZERO && playing(shared) {
                interrupt(shared);
            }
            speech += elapsed;
            quiet = Duration::ZERO;
        } else {
            quiet += elapsed;
        }
        if speech > Duration::ZERO {
            turn.extend_from_slice(&frame);
        }
        if speech > Duration::ZERO && quiet >= PAUSE {
            if speech >= SPEECH {
                hear(&turn, engine, shared);
            }
            turn.clear();
            speech = Duration::ZERO;
        }
    }
}

fn hear(turn: &[i16], engine: &Mutex<Engine>, shared: &Shared) {
    let file = std::env::temp_dir().join(format!(
        "messagent-heard-{}-{}.wav",
        std::process::id(),
        Timestamp::now().as_nanosecond()
    ));
    let result = fs::write(&file, wav(&downsample(turn), RATE / 3))
        .map_err(anyhow::Error::from)
        .and_then(|()| engine.lock().expect("no panics").transcribe(&file));
    fs::remove_file(&file).ok();
    match result {
        Ok(text) if text.is_empty() => {}
        Ok(text) => shared.append(Entry {
            time: Timestamp::now(),
            from: Party::User,
            name: None,
            message: None,
            spoken: true,
            text,
        }),
        Err(error) => shared.notice(format!("A turn of the user's couldn't be heard: {error:#}")),
    }
}

/// Turns what the agent says into clips, queued for the player.
fn talk(said: mpsc::Receiver<String>, engine: &Mutex<Engine>, shared: &Shared) {
    for text in said {
        let index = {
            let mut sayings = shared.sayings.lock().expect("no panics");
            sayings.push(Saying {
                text: text.clone(),
                played: Duration::ZERO,
                over: false,
            });
            sayings.len() - 1
        };
        shared.append(Entry {
            time: Timestamp::now(),
            from: Party::Agent,
            name: None,
            message: None,
            spoken: true,
            text: text.clone(),
        });
        let result = engine.lock().expect("no panics").speak(&text, |clip| {
            if shared.sayings.lock().expect("no panics")[index].over {
                fs::remove_file(&clip).ok();
            } else {
                shared.queue.lock().expect("no panics").push((index, clip));
            }
        });
        if let Err(error) = result {
            shared.notice(format!("Your words couldn't be spoken: {error:#}"));
        }
        // The last clip is queued: the saying is over once the queue holds none of it.
        let queued = shared
            .queue
            .lock()
            .expect("no panics")
            .iter()
            .any(|(of, _)| *of == index);
        let current = *shared.current.lock().expect("no panics") == Some(index);
        if !queued && !current {
            shared.sayings.lock().expect("no panics")[index].over = true;
        }
    }
}

/// Plays the queued clips in turn, until the call ends.
fn play(speaker: &dyn Speaker, shared: &Shared) {
    while !shared.ended.load(Ordering::SeqCst) {
        let next = {
            let mut queue = shared.queue.lock().expect("no panics");
            let next = (!queue.is_empty()).then(|| queue.remove(0));
            *shared.current.lock().expect("no panics") = next.as_ref().map(|(index, _)| *index);
            next
        };
        let Some((index, file)) = next else {
            thread::sleep(FRAME_TIME);
            continue;
        };
        let clip = fs::read(&file)
            .map_err(anyhow::Error::from)
            .and_then(|wav| speaker.play(wav));
        fs::remove_file(&file).ok();
        let mut position = Duration::ZERO;
        let mut cut = false;
        match clip {
            Ok(clip) => {
                while let Some(now) = clip.position() {
                    position = now;
                    cut = shared.interrupted.swap(false, Ordering::SeqCst);
                    if cut || shared.ended.load(Ordering::SeqCst) {
                        clip.stop();
                        break;
                    }
                    thread::sleep(FRAME_TIME);
                }
            }
            Err(error) => shared.notice(format!("A clip couldn't be played: {error:#}")),
        }
        let queued = shared
            .queue
            .lock()
            .expect("no panics")
            .iter()
            .any(|(of, _)| *of == index);
        *shared.current.lock().expect("no panics") = None;
        let mut sayings = shared.sayings.lock().expect("no panics");
        let saying = &mut sayings[index];
        saying.played += position;
        if cut {
            saying.over = true;
            let report = cut_off(saying);
            drop(sayings);
            shared.notice(report);
        } else if !queued {
            saying.over = true;
        }
    }
}

/// Whether the agent is saying something, or about to.
fn playing(shared: &Shared) -> bool {
    shared
        .sayings
        .lock()
        .expect("no panics")
        .iter()
        .any(|saying| !saying.over)
}

/// The user speaks over the agent: what is queued goes, and the player stops.
fn interrupt(shared: &Shared) {
    let dropped: Vec<(usize, PathBuf)> =
        shared.queue.lock().expect("no panics").drain(..).collect();
    for (_, file) in &dropped {
        fs::remove_file(file).ok();
    }
    let current = *shared.current.lock().expect("no panics");
    shared
        .interrupted
        .store(current.is_some(), Ordering::SeqCst);
    // The saying that plays is reported by the player, once it knows how far it got.
    let mut reports = Vec::new();
    for (index, saying) in shared
        .sayings
        .lock()
        .expect("no panics")
        .iter_mut()
        .enumerate()
    {
        if !saying.over && Some(index) != current {
            saying.over = true;
            reports.push(cut_off(saying));
        }
    }
    for report in reports {
        shared.notice(report);
    }
}

fn cut_off(saying: &Saying) -> String {
    format!(
        "The user spoke over you: of \"{}\", they heard about the first {:.1} s.",
        saying.text,
        saying.played.as_secs_f64()
    )
}

fn rms(frame: &[i16]) -> f64 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame.iter().map(|&s| f64::from(s).powi(2)).sum();
    (sum / frame.len() as f64).sqrt()
}

/// From `RATE` to a third of it, which speech recognition takes, by averaging.
fn downsample(samples: &[i16]) -> Vec<i16> {
    samples
        .chunks(3)
        .map(|chunk| (chunk.iter().map(|&s| i32::from(s)).sum::<i32>() / chunk.len() as i32) as i16)
        .collect()
}

/// A WAV file of mono 16-bit PCM.
pub fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}
