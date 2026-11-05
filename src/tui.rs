mod browser;
use crate::{
    config::Config,
    daemon::{self, Subscription},
    domain::{Platform, PlaybackState, Snapshot},
    protocol::AppCommand,
    services::ServiceCommand,
};
use anyhow::{Result, ensure};
use browser::Browser;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{self, IsTerminal},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet};

const EN: [&str; 5] = ["Search", "Playlists", "Charts", "My Library", "Downloads"];
const ZH: [&str; 5] = ["搜索", "歌单", "排行榜", "我的列表", "下载"];
enum Update {
    Snapshot(Snapshot),
    Error(String),
    Query {
        index: usize,
        version: u64,
        result: Result<Value, String>,
    },
    Config(Config),
    Message(String),
    Lyrics {
        track: String,
        document: crate::lyrics::Lyrics,
    },
}
enum Action {
    Control(AppCommand),
    Query {
        index: usize,
        version: u64,
        command: ServiceCommand,
    },
    Service(ServiceCommand),
    Lyrics(String),
}

pub async fn run() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "The TUI needs an interactive terminal; use `yuin --help` for command-line options"
    );
    let (mut subscription, initial) = Subscription::connect(true).await?;
    let value = daemon::call_value(
        AppCommand::Service {
            command: ServiceCommand::ConfigGet,
        },
        false,
    )
    .await?;
    let config: Config = serde_json::from_value(value["config"].clone())?;
    let (actions, mut action_rx) = mpsc::channel::<Action>(32);
    let (updates, mut update_rx) = mpsc::channel(64);
    let worker = tokio::spawn(async move {
        let mut config_poll = tokio::time::interval(Duration::from_secs(3));
        let mut jobs = JoinSet::new();
        let mut queries = HashMap::<usize, tokio::task::AbortHandle>::new();
        loop {
            tokio::select! {
                _=config_poll.tick()=>{
                    jobs.spawn(async {match daemon::call_value(AppCommand::Service{command:ServiceCommand::ConfigGet},false).await.and_then(|v|serde_json::from_value::<Config>(v["config"].clone()).map_err(Into::into)){Ok(config)=>Update::Config(config),Err(error)=>Update::Error(error.to_string())}});
                }
                action=action_rx.recv()=>{
                    let Some(action)=action else{break;};
                    match action {
                        Action::Lyrics(track)=>{
                            if let Some(previous)=queries.remove(&99){previous.abort();}
                            let handle=jobs.spawn(async move{match daemon::call_value(AppCommand::Service{command:ServiceCommand::Lyrics{track:track.clone(),refresh:false}},false).await.and_then(|v|serde_json::from_value::<crate::lyrics::Lyrics>(v["lyrics"].clone()).map_err(Into::into)){Ok(document)=>Update::Lyrics{track,document},Err(error)=>Update::Error(error.to_string())}});queries.insert(99,handle);
                        }
                        Action::Control(command)=>{let command=if matches!(command,AppCommand::Play{..}|AppCommand::PlayEntry{..}|AppCommand::Resume|AppCommand::Next|AppCommand::Previous){AppCommand::Detached{command:Box::new(command)}}else{command};jobs.spawn(async move{match daemon::call(command,false).await{Ok(snapshot)=>Update::Snapshot(snapshot),Err(error)=>Update::Error(error.to_string())}});},
                        Action::Service(command)=>{jobs.spawn(async move{match daemon::call_value(AppCommand::Service{command},false).await{Ok(value)=>if value["config"].is_object(){match serde_json::from_value(value["config"].clone()){Ok(config)=>Update::Config(config),Err(error)=>Update::Error(error.to_string())}}else{Update::Message("Done".into())},Err(error)=>Update::Error(error.to_string())}});},
                        Action::Query{index,version,command}=>{
                            if let Some(previous)=queries.remove(&index){previous.abort();}
                            let handle=jobs.spawn(async move{let result=daemon::call_value(AppCommand::Service{command},false).await.map_err(|e|e.to_string());Update::Query{index,version,result}});queries.insert(index,handle);
                        }
                    }
                }
                Some(result)=jobs.join_next(),if !jobs.is_empty()=>{if let Ok(update)=result&& updates.send(update).await.is_err(){break;}},
                snapshot=subscription.next()=>match snapshot{
                    Ok(snapshot)=>{if updates.send(Update::Snapshot(snapshot)).await.is_err(){break;}},
                    Err(error)=>{
                        let _=updates.send(Update::Error(error.to_string())).await;
                        loop {tokio::time::sleep(Duration::from_secs(1)).await;if let Ok((next,snapshot))=Subscription::connect(false).await{subscription=next;let _=updates.send(Update::Snapshot(snapshot)).await;break;}
                            if action_rx.is_closed(){return;}}
                    }
                }
            }
        }
        jobs.abort_all();
    });
    let mut view = View {
        lyrics_mode: false,
        lyric_track: None,
        lyrics: None,
        snapshot: initial,
        page: 1,
        focus: 1,
        queue: ListState::default().with_selected(Some(0)),
        message: String::new(),
        browser: Browser::new(config.platform, config.language == "en"),
        config,
    };
    view.refresh(&actions, false);
    let result = tokio::task::spawn_blocking(move || {
        ratatui::run(|terminal| -> io::Result<()> {
            let mut dirty = true;
            let mut refresh_at = std::time::Instant::now();
            loop {
                while let Ok(update) = update_rx.try_recv() {
                    match update {
                        Update::Snapshot(snapshot) => {
                            if snapshot.instance_id == view.snapshot.instance_id
                                && snapshot.sequence < view.snapshot.sequence
                            {
                                continue;
                            }
                            let selected = view
                                .queue
                                .selected()
                                .and_then(|i| view.snapshot.queue.get(i))
                                .map(|e| e.id);
                            let index = selected
                                .and_then(|id| snapshot.queue.iter().position(|e| e.id == id))
                                .or({
                                    if snapshot.queue.is_empty() {
                                        None
                                    } else {
                                        Some(0)
                                    }
                                });
                            view.queue.select(index);
                            view.snapshot = snapshot;
                            if view.lyrics_mode {
                                view.request_lyrics(&actions);
                            }
                        }
                        Update::Query {
                            index,
                            version,
                            result,
                        } => {
                            if view.browser.routes[index].version != version {
                                continue;
                            }
                            match result {
                                Ok(value) => {
                                    view.browser.apply(index, version, value);
                                }
                                Err(error) => {
                                    view.browser.routes[index].loading = false;
                                    view.message = error;
                                }
                            }
                        }
                        Update::Config(config) => {
                            let language = config.language == "en";
                            if view.browser.platform != config.platform
                                || view.browser.english != language
                            {
                                view.browser = Browser::new(config.platform, language);
                                view.refresh(&actions, false);
                            }
                            view.config = config;
                        }
                        Update::Lyrics { track, document } => {
                            if view.lyric_track.as_deref() == Some(&track) {
                                view.lyrics = Some(document);
                            }
                        }
                        Update::Error(error) | Update::Message(error) => view.message = error,
                    }
                    dirty = true;
                }
                if ((view.page == 3
                    && matches!(view.browser.routes[3].content.kind, browser::Kind::Library))
                    || view.page == 4)
                    && view.browser.routes[view.page].content.page == 1
                    && refresh_at.elapsed() >= Duration::from_secs(1)
                    && !view.browser.routes[view.page].loading
                {
                    view.refresh(&actions, false);
                    refresh_at = std::time::Instant::now();
                }
                if dirty {
                    terminal.draw(|frame| draw(frame, &mut view))?;
                    dirty = false;
                }
                if !event::poll(Duration::from_millis(80))? {
                    continue;
                }
                let key = match event::read()? {
                    Event::Key(key) if key.is_press() => key,
                    Event::Resize(_, _) => {
                        dirty = true;
                        continue;
                    }
                    _ => continue,
                };
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if let Some(input) = &mut view.browser.input {
                    match key.code {
                        KeyCode::Esc => view.browser.input = None,
                        KeyCode::Enter => {
                            let query = input.clone();
                            view.browser.input = None;
                            view.page = 0;
                            view.focus = 1;
                            view.browser.routes[0].query = query;
                            view.refresh(&actions, false);
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char(c) => input.push(c),
                        _ => {}
                    }
                    dirty = true;
                    continue;
                }
                let mut command = None;
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Tab => view.focus = (view.focus + 1) % 3,
                    KeyCode::BackTab => view.focus = (view.focus + 2) % 3,
                    KeyCode::Char('/') => {
                        view.browser.input = Some(view.browser.routes[0].query.clone());
                    }
                    KeyCode::Char(number @ '1'..='5') => {
                        view.page = number as usize - '1' as usize;
                        view.focus = 1;
                        if !view.browser.routes[view.page].content.loaded {
                            view.refresh(&actions, false);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => view.navigate(1, &actions),
                    KeyCode::Up | KeyCode::Char('k') => view.navigate(-1, &actions),
                    KeyCode::Enter => {
                        if view.focus == 2 {
                            command = view
                                .queue
                                .selected()
                                .and_then(|i| view.snapshot.queue.get(i))
                                .map(|e| AppCommand::PlayEntry { id: e.id });
                        } else if view.focus == 1 {
                            if let Some(id) = view.browser.track_id(view.page) {
                                command = Some(AppCommand::Play { path: id });
                            } else if view.browser.enter(view.page) {
                                view.refresh(&actions, false);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        view.browser.back(view.page);
                    }
                    KeyCode::Char('l') => {
                        view.lyrics_mode = !view.lyrics_mode;
                        if view.lyrics_mode {
                            view.request_lyrics(&actions);
                        }
                    }
                    KeyCode::Char('r') => view.refresh(&actions, false),
                    KeyCode::Char(']') => {
                        if view.browser.routes[view.page].content.has_more
                            && !view.browser.routes[view.page].loading
                        {
                            view.refresh(&actions, true);
                        }
                    }
                    KeyCode::Char('P') => {
                        let next = match view.browser.platform {
                            Platform::Wy => Platform::Kw,
                            Platform::Kw => Platform::Tx,
                            Platform::Tx => Platform::Kg,
                            Platform::Kg => Platform::Mg,
                            Platform::Mg => Platform::Wy,
                        };
                        view.send(
                            &actions,
                            Action::Service(ServiceCommand::ConfigSet {
                                key: "platform".into(),
                                value: next.to_string(),
                            }),
                        );
                    }
                    KeyCode::Char('a') => {
                        if let Some(id) = view.browser.track_id(view.page) {
                            command = Some(AppCommand::QueueAdd { path: id });
                        }
                    }
                    KeyCode::Char('f') => {
                        if let Some(track) = view.browser.track_id(view.page) {
                            view.send(
                                &actions,
                                Action::Service(ServiceCommand::Favorite { track }),
                            );
                        }
                    }
                    KeyCode::Char('d') => {
                        if let Some(id) = view.browser.track_id(view.page) {
                            view.send(
                                &actions,
                                Action::Service(ServiceCommand::Download {
                                    track: id,
                                    quality: None,
                                }),
                            );
                        }
                    }
                    KeyCode::Char('x') => {
                        if view.page == 4
                            && let Some(id) = view
                                .browser
                                .selected(4)
                                .and_then(|v| v["id"].as_str())
                                .and_then(|s| s.parse().ok())
                        {
                            view.send(
                                &actions,
                                Action::Service(ServiceCommand::DownloadCancel { id }),
                            );
                        }
                    }
                    KeyCode::Char(' ') => {
                        command = Some(
                            if matches!(
                                view.snapshot.state,
                                PlaybackState::Playing | PlaybackState::Buffering
                            ) {
                                AppCommand::Pause
                            } else {
                                AppCommand::Resume
                            },
                        )
                    }
                    KeyCode::Char('n') => command = Some(AppCommand::Next),
                    KeyCode::Char('p') => command = Some(AppCommand::Previous),
                    KeyCode::Left => {
                        command = Some(AppCommand::Seek {
                            seconds: (view.snapshot.position - 5.0).max(0.0),
                        })
                    }
                    KeyCode::Right => {
                        command = Some(AppCommand::Seek {
                            seconds: view.snapshot.position + 5.0,
                        })
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        command = Some(AppCommand::Volume {
                            percent: view.snapshot.volume.saturating_add(5).min(100),
                        })
                    }
                    KeyCode::Char('-') => {
                        command = Some(AppCommand::Volume {
                            percent: view.snapshot.volume.saturating_sub(5),
                        })
                    }
                    KeyCode::Char('s') => command = Some(AppCommand::Stop),
                    _ => {}
                }
                if let Some(command) = command {
                    view.send(&actions, Action::Control(command));
                }
                dirty = true;
            }
            Ok(())
        })
    })
    .await;
    worker.abort();
    result??;
    Ok(())
}
struct View {
    lyrics_mode: bool,
    lyric_track: Option<String>,
    lyrics: Option<crate::lyrics::Lyrics>,
    snapshot: Snapshot,
    page: usize,
    focus: usize,
    queue: ListState,
    message: String,
    browser: Browser,
    config: Config,
}
impl View {
    fn request_lyrics(&mut self, actions: &mpsc::Sender<Action>) {
        let current = self.snapshot.current_entry().map(|e| e.track.id.clone());
        if current != self.lyric_track {
            self.lyrics = None;
            self.lyric_track = current.clone();
            if let Some(id) = current {
                self.send(actions, Action::Lyrics(id));
            }
        }
    }
    fn send(&mut self, actions: &mpsc::Sender<Action>, action: Action) {
        self.message = if actions.try_send(action).is_ok() {
            if self.browser.english {
                "Command sent"
            } else {
                "已发送操作"
            }
            .into()
        } else {
            if self.browser.english {
                "Service unavailable or busy"
            } else {
                "服务不可用或繁忙"
            }
            .into()
        };
    }
    fn refresh(&mut self, actions: &mpsc::Sender<Action>, append: bool) {
        if let Some((version, command)) = self.browser.query(self.page, append) {
            self.send(
                actions,
                Action::Query {
                    index: self.page,
                    version,
                    command,
                },
            );
        }
    }
    fn navigate(&mut self, delta: isize, actions: &mpsc::Sender<Action>) {
        match self.focus {
            0 => {
                self.page = (self.page as isize + delta).rem_euclid(5) as usize;
                if !self.browser.routes[self.page].content.loaded {
                    self.refresh(actions, false);
                }
            }
            1 => self.browser.navigate(self.page, delta),
            _ => {
                if !self.snapshot.queue.is_empty() {
                    self.queue.select(Some(
                        self.queue
                            .selected()
                            .unwrap_or(0)
                            .saturating_add_signed(delta)
                            .min(self.snapshot.queue.len() - 1),
                    ));
                }
            }
        }
    }
}
fn draw(frame: &mut Frame, view: &mut View) {
    let english = view.browser.english;
    let accent = Color::Rgb(255, 45, 85);
    let style = match view.config.theme.as_str() {
        "dark" => Style::default().fg(Color::White).bg(Color::Black),
        "light" => Style::default().fg(Color::Black).bg(Color::White),
        _ => Style::default(),
    };
    frame.render_widget(Block::new().style(style), frame.area());
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(2),
        Constraint::Length(3),
        Constraint::Length(if view.browser.input.is_some() { 3 } else { 2 }),
    ])
    .split(frame.area());
    let source: String = view
        .config
        .source
        .map_or("—".into(), |id| id.to_string()[..8].into());
    frame.render_widget(
        Paragraph::new(format!(
            " Yuin · 余音  |  {}  |  {}: {source}",
            view.browser.platform,
            if english { "Source" } else { "音乐源" }
        ))
        .style(Style::default().fg(accent))
        .block(Block::bordered()),
        rows[0],
    );
    let columns = Layout::horizontal([
        Constraint::Length(if frame.area().width >= 70 { 18 } else { 12 }),
        Constraint::Min(8),
    ])
    .split(rows[1]);
    let pages = if english { EN } else { ZH };
    let items = pages
        .iter()
        .enumerate()
        .map(|(i, title)| ListItem::new(format!("{} {title}", i + 1)));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(if english { "Browse" } else { "浏览" }))
            .highlight_style(Style::default().fg(accent))
            .highlight_symbol(if view.focus == 0 { "› " } else { "  " }),
        columns[0],
        &mut ListState::default().with_selected(Some(view.page)),
    );
    let (content, queue) = if frame.area().width >= 100 {
        let split =
            Layout::horizontal([Constraint::Min(20), Constraint::Length(30)]).split(columns[1]);
        (Some(split[0]), Some(split[1]))
    } else if view.focus == 2 {
        (None, Some(columns[1]))
    } else {
        (Some(columns[1]), None)
    };
    if let Some(area) = content {
        if view.lyrics_mode {
            if let Some(lyrics) = &view.lyrics {
                if lyrics.lines.is_empty() {
                    frame.render_widget(
                        Paragraph::new(lyrics.plain.clone()).block(
                            Block::bordered().title(if english { "Lyrics" } else { "歌词" }),
                        ),
                        area,
                    );
                } else {
                    let active = lyrics.active(view.snapshot.position);
                    let items = lyrics.lines.iter().map(|line| {
                        ListItem::new(format!(
                            "{}{}",
                            line.text,
                            line.translation
                                .as_ref()
                                .map(|t| format!("\n{t}"))
                                .unwrap_or_default()
                        ))
                    });
                    frame.render_stateful_widget(
                        List::new(items)
                            .block(Block::bordered().title(if english {
                                "Lyrics"
                            } else {
                                "歌词"
                            }))
                            .highlight_style(Style::default().fg(accent)),
                        area,
                        &mut ListState::default().with_selected(active),
                    );
                }
            } else {
                frame.render_widget(
                    Paragraph::new(if english {
                        "Loading lyrics…"
                    } else {
                        "正在加载歌词…"
                    })
                    .block(Block::bordered()),
                    area,
                );
            }
        } else {
            view.browser
                .draw(frame, area, view.page, view.focus == 1, accent);
        }
    }
    if let Some(area) = queue {
        let items = view.snapshot.queue.iter().map(|e| {
            ListItem::new(format!(
                "{} {}",
                if view.snapshot.current == Some(e.id) {
                    "▶"
                } else {
                    " "
                },
                e.track.title
            ))
        });
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(if english { "Queue" } else { "队列" }))
                .highlight_style(Style::default().fg(accent))
                .highlight_symbol("› "),
            area,
            &mut view.queue,
        );
    }
    let state = if english {
        match view.snapshot.state {
            PlaybackState::Idle => "Idle",
            PlaybackState::Resolving => "Resolving",
            PlaybackState::Buffering => "Buffering",
            PlaybackState::Playing => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Ended => "Ended",
            PlaybackState::Error => "Error",
        }
    } else {
        match view.snapshot.state {
            PlaybackState::Idle => "空闲",
            PlaybackState::Resolving => "解析中",
            PlaybackState::Buffering => "缓冲中",
            PlaybackState::Playing => "播放中",
            PlaybackState::Paused => "已暂停",
            PlaybackState::Ended => "已结束",
            PlaybackState::Error => "错误",
        }
    };
    let mode = if english {
        view.snapshot.mode.as_str()
    } else {
        match view.snapshot.mode {
            crate::domain::PlaybackMode::Sequential => "顺序",
            crate::domain::PlaybackMode::Shuffle => "随机",
            crate::domain::PlaybackMode::RepeatOne => "单曲循环",
        }
    };
    let title = view.snapshot.current_entry().map_or(
        if english {
            "No audio selected"
        } else {
            "未选择歌曲"
        },
        |e| e.track.title.as_str(),
    );
    let duration = view
        .snapshot
        .duration
        .map_or("—".into(), |n| format!("{n:.0}s"));
    frame.render_widget(
        Paragraph::new(format!(
            " {title} | {state} {:.0}s / {duration} | {} {}% · {mode}",
            view.snapshot.position,
            if english { "Volume" } else { "音量" },
            view.snapshot.volume
        ))
        .block(Block::bordered()),
        rows[2],
    );
    if let Some(input) = &view.browser.input {
        frame.render_widget(
            Paragraph::new(format!(" / {input}")).block(Block::bordered().title(if english {
                "Search · Enter to submit · Esc to cancel"
            } else {
                "搜索 · Enter 提交 · Esc 取消"
            })),
            rows[3],
        );
    } else {
        let keys = if english {
            " q: leave  /: search  Tab: focus  Enter: open/play  ]: more  a: queue  d: download  f: favorite  P: platform  Space: pause"
        } else {
            " q 退出  / 搜索  Tab 焦点  Enter 打开/播放  ] 更多  a 入队  d 下载  f 收藏  P 平台  Space 暂停"
        };
        let message = view.snapshot.error.as_deref().unwrap_or(&view.message);
        frame.render_widget(Paragraph::new(format!("{keys}\n {message}")), rows[3]);
    }
}
