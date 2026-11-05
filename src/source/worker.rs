use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rquickjs::{Context as JsContext, Function, Runtime};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    io::{BufRead, Read, Write},
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const HTTP_LIMIT: usize = 8_000_000;
enum Input {
    Command(Value),
    Http(u64, Result<Value, String>),
    Closed,
}

pub fn run(path: &Path) -> Result<()> {
    let script = std::fs::read(path)?;
    ensure!(
        !script.is_empty() && script.len() <= super::SCRIPT_LIMIT,
        "Invalid script size"
    );
    let script = String::from_utf8(script)?;
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(128 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    let start = Instant::now();
    let deadline = Arc::new(AtomicU64::new(15_000));
    let closed = Arc::new(AtomicBool::new(false));
    let interrupt_deadline = deadline.clone();
    let interrupt_closed = closed.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupt_closed.load(Ordering::Relaxed)
            || start.elapsed().as_millis() as u64 >= interrupt_deadline.load(Ordering::Relaxed)
    })));
    let context = JsContext::full(&runtime)?;
    let (sender, receiver) = mpsc::sync_channel::<Input>(32);
    let input_sender = sender.clone();
    let input_closed = closed.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            let mut frame = Vec::new();
            let read = stdin
                .by_ref()
                .take(crate::protocol::MAX_FRAME as u64 + 1)
                .read_until(b'\n', &mut frame);
            if !matches!(read,Ok(n) if n>0 && n<=crate::protocol::MAX_FRAME) {
                break;
            }
            match serde_json::from_slice(&frame) {
                Ok(value) => {
                    if input_sender.send(Input::Command(value)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        input_closed.store(true, Ordering::Relaxed);
        let _ = input_sender.send(Input::Closed);
    });
    let emitted = Rc::new(RefCell::new(VecDeque::<String>::new()));
    let timers = Rc::new(RefCell::new(HashMap::<u64, Instant>::new()));
    let cancellations: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
    let active_http = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    context
        .with(|ctx| -> rquickjs::Result<()> {
            let output = emitted.clone();
            ctx.globals().set(
                "__emit",
                Function::new(ctx.clone(), move |value: String| {
                    if value.len() <= crate::protocol::MAX_FRAME && output.borrow().len() < 128 {
                        output.borrow_mut().push_back(value);
                    }
                })?,
            )?;
            ctx.globals().set(
                "__native",
                Function::new(ctx.clone(), |op: String, args: String| {
                    super::crypto::call(&op, &args)
                })?,
            )?;
            let schedule = timers.clone();
            ctx.globals().set(
                "__schedule",
                Function::new(ctx.clone(), move |id: u64, delay: f64| {
                    if schedule.borrow().len() < 4096 {
                        schedule.borrow_mut().insert(
                            id,
                            Instant::now()
                                + Duration::from_millis(delay.clamp(0.0, 86_400_000.0) as u64),
                        );
                    }
                })?,
            )?;
            let cancel = cancellations.clone();
            ctx.globals().set(
                "__cancelHTTP",
                Function::new(ctx.clone(), move |id: u64| {
                    if let Some(flag) = cancel.lock().unwrap().remove(&id) {
                        flag.store(true, Ordering::Relaxed);
                    }
                })?,
            )?;
            let http_sender = sender.clone();
            let http_cancel = cancellations.clone();
            ctx.globals().set(
                "__http",
                Function::new(ctx.clone(), move |id: u64, url: String, options: String| {
                    if active_http.fetch_add(1, Ordering::Relaxed) >= 8 {
                        active_http.fetch_sub(1, Ordering::Relaxed);
                        let _ = http_sender
                            .try_send(Input::Http(id, Err("Too many source HTTP requests".into())));
                        return;
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    http_cancel.lock().unwrap().insert(id, cancel.clone());
                    let client = client.clone();
                    let sender = http_sender.clone();
                    let active = active_http.clone();
                    std::thread::spawn(move || {
                        let result = http(&client, &url, &options, &cancel)
                            .map_err(|_| "Source HTTP request failed".to_owned());
                        active.fetch_sub(1, Ordering::Relaxed);
                        if !cancel.load(Ordering::Relaxed) {
                            let _ = sender.send(Input::Http(id, result));
                        }
                    });
                })?,
            )?;
            ctx.eval::<(), _>(include_str!("bridge.js"))?;
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("Cannot initialize LX bridge: {e}"))?;
    let mut pending: Option<(Value, Instant)> = None;
    loop {
        if closed.load(Ordering::Relaxed) {
            break;
        }
        deadline.store(
            (start.elapsed() + Duration::from_secs(2)).as_millis() as u64,
            Ordering::Relaxed,
        );
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(Input::Closed) => break,
            Ok(Input::Command(mut command)) => {
                ensure!(pending.is_none(), "Worker received overlapping commands");
                let init = command["op"] == "init";
                let duration = if init {
                    Duration::from_secs(15)
                } else {
                    Duration::from_secs(65)
                };
                deadline.store(
                    (start.elapsed() + duration).as_millis() as u64,
                    Ordering::Relaxed,
                );
                if init {
                    command["script"] = json!(script);
                    command["meta"]["rawScript"] = json!(script);
                }
                let id = command["id"].clone();
                pending = Some((id.clone(), Instant::now() + duration));
                if evaluate(
                    &context,
                    &format!(
                        "__command(JSON.parse({}));",
                        serde_json::to_string(&command.to_string())?
                    ),
                )
                .is_err()
                {
                    emit_error(
                        &id,
                        "Source script evaluation failed or exceeded resource limit",
                    )?;
                    pending = None;
                }
            }
            Ok(Input::Http(id, result)) => {
                cancellations.lock().unwrap().remove(&id);
                let (error, response) = match result {
                    Ok(response) => (Value::Null, response),
                    Err(error) => (json!(error), Value::Null),
                };
                if evaluate(&context, &format!("__httpReply({id},{error},{response});")).is_err()
                    && let Some((id, _)) = &pending
                {
                    emit_error(id, "Source HTTP callback failed")?;
                    pending = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some((id, end)) = &pending {
            if Instant::now() >= *end {
                emit_error(id, "Source request timed out")?;
                break;
            }
            deadline.store(
                (start.elapsed() + end.saturating_duration_since(Instant::now())).as_millis()
                    as u64,
                Ordering::Relaxed,
            );
        }
        let due = timers
            .borrow()
            .iter()
            .filter(|(_, at)| **at <= Instant::now())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in due {
            timers.borrow_mut().remove(&id);
            if evaluate(&context, &format!("__timer({id});")).is_err()
                && let Some((id, _)) = &pending
            {
                emit_error(id, "Source timer callback failed")?;
                pending = None;
            }
        }
        for _ in 0..1024 {
            match runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => {
                    if let Some((id, _)) = &pending {
                        emit_error(id, "Source Promise failed")?;
                        pending = None;
                    }
                    break;
                }
            }
        }
        while let Some(message) = emitted.borrow_mut().pop_front() {
            let value: Value = serde_json::from_str(&message)?;
            if let Some((id, _)) = &pending
                && value["id"] == *id
            {
                pending = None;
            }
            println!("{message}");
            std::io::stdout().flush()?;
        }
    }
    Ok(())
}

fn evaluate(context: &JsContext, script: &str) -> Result<()> {
    context
        .with(|ctx| ctx.eval::<(), _>(script.as_bytes()))
        .map_err(|_| anyhow::anyhow!("JavaScript exception"))
}
fn emit_error(id: &Value, message: &str) -> Result<()> {
    println!("{}", json!({"id":id,"error":message}));
    std::io::stdout().flush()?;
    Ok(())
}

fn http(
    client: &reqwest::blocking::Client,
    url: &str,
    options: &str,
    cancel: &AtomicBool,
) -> Result<Value> {
    let options: Value = serde_json::from_str(options)?;
    let url = crate::http::url(url)?;
    let method = options["method"]
        .as_str()
        .unwrap_or("GET")
        .to_uppercase()
        .parse::<reqwest::Method>()?;
    let mut request = client.request(method, url).timeout(Duration::from_millis(
        options["timeout"]
            .as_u64()
            .unwrap_or(60000)
            .clamp(1000, 60000),
    ));
    if let Some(headers) = options["headers"].as_object() {
        for (key, value) in headers {
            request = request.header(
                key,
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
    }
    if let Some(body) = options.get("body") {
        if let Some(text) = body.as_str() {
            request = request.body(text.to_string());
        } else if body["type"] == "Buffer" {
            request = request.body(serde_json::from_value::<Vec<u8>>(body["data"].clone())?);
        } else {
            request = request.json(body);
        }
    } else if let Some(form) = options["form"].as_object() {
        let pairs = form
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string()),
                )
            })
            .collect::<Vec<_>>();
        request = request.form(&pairs);
    } else if let Some(form) = options["formData"].as_object() {
        let boundary = format!("yuin-{}", uuid::Uuid::new_v4());
        let mut body = String::new();
        for (k, v) in form {
            ensure!(!k.contains(['\r', '\n', '"']), "Invalid form name");
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{}\r\n",
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            ));
        }
        body.push_str(&format!("--{boundary}--\r\n"));
        request = request
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
    }
    let response = request.send()?;
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= HTTP_LIMIT as u64),
        "Source HTTP response too large"
    );
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), json!(v))))
        .collect::<serde_json::Map<_, _>>();
    let mut data = Vec::new();
    let mut response = response.take(HTTP_LIMIT as u64 + 1);
    let mut block = [0; 8192];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "Cancelled");
        let n = response.read(&mut block)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&block[..n]);
        ensure!(data.len() <= HTTP_LIMIT, "Source HTTP response too large");
    }
    let body = serde_json::from_slice::<Value>(&data)
        .unwrap_or_else(|_| json!(String::from_utf8_lossy(&data)));
    Ok(
        json!({"statusCode":status.as_u16(),"statusMessage":status.canonical_reason().unwrap_or(""),"headers":headers,"raw":STANDARD.encode(data),"body":body}),
    )
}
