mod browser;
mod cover;
mod theme;
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
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{self, IsTerminal},
    time::Duration,
};
use theme::Theme;
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
    SavedPlaylist {
        count: usize,
        already: bool,
    },
    Lyrics {
        track: String,
        document: crate::lyrics::Lyrics,
    },
    Cover {
        slot: cover::Slot,
        key: String,
        png: cover::Image,
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
    Cover {
        slot: cover::Slot,
        target: Box<cover::Target>,
    },
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
        let mut cover_cache = cover::Cache::default();
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
                        Action::Cover{slot,target}=>{
                            if let Some(previous)=queries.remove(&slot.request_id()){previous.abort();}
                            let key=target.key.clone();
                            if let Some(png)=cover_cache.get(&key) {
                                if updates.send(Update::Cover{slot,key,png}).await.is_err(){break;}
                            } else {
                                let handle=jobs.spawn(async move { let png=cover::fetch(*target).await.ok().map(std::sync::Arc::new); Update::Cover{slot,key,png} }); queries.insert(slot.request_id(),handle);
                            }
                        }
                        Action::Control(command)=>{let command=if matches!(command,AppCommand::Play{..}|AppCommand::PlayEntry{..}|AppCommand::PlayCollection{..}|AppCommand::PlayLibrary{..}|AppCommand::Resume|AppCommand::Next|AppCommand::Previous){AppCommand::Detached{command:Box::new(command)}}else{command};jobs.spawn(async move{match daemon::call(command,false).await{Ok(snapshot)=>Update::Snapshot(snapshot),Err(error)=>Update::Error(error.to_string())}});},
                        Action::Service(command)=>{jobs.spawn(async move{match daemon::call_value(AppCommand::Service{command},false).await{Ok(value)=>if value["config"].is_object(){match serde_json::from_value(value["config"].clone()){Ok(config)=>Update::Config(config),Err(error)=>Update::Error(error.to_string())}}else if let Some(count)=value["playlist"]["count"].as_u64(){Update::SavedPlaylist{count:count as usize,already:value["already_saved"]==true}}else{Update::Message("Done".into())},Err(error)=>Update::Error(error.to_string())}});},
                        Action::Query{index,version,command}=>{
                            if let Some(previous)=queries.remove(&index){previous.abort();}
                            let handle=jobs.spawn(async move{let result=daemon::call_value(AppCommand::Service{command},false).await.map_err(|e|e.to_string());Update::Query{index,version,result}});queries.insert(index,handle);
                        }
                    }
                }
                Some(result)=jobs.join_next(),if !jobs.is_empty()=>{
                    if let Ok(update)=result {
                        if let Update::Cover{key,png,..}=&update {
                            cover_cache.insert(key.clone(),png.clone());
                        }
                        if updates.send(update).await.is_err(){break;}
                    }
                },
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
        cover_supported: cover::supported(),
        preview: cover::State::default(),
        playing_cover: cover::State::default(),
    };
    view.refresh(&actions, false);
    let result = tokio::task::spawn_blocking(move || {
        ratatui::run(|terminal| -> io::Result<()> {
            let mut dirty = true;
            let mut preview_renderer = cover::Renderer::new(cover::Slot::Preview);
            let mut playing_renderer = cover::Renderer::new(cover::Slot::Playing);
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
                        Update::Cover { slot, key, png } => {
                            match slot {
                                cover::Slot::Preview => view.preview.apply(&key, png),
                                cover::Slot::Playing => view.playing_cover.apply(&key, png),
                            };
                        }
                        Update::SavedPlaylist { count, already } => {
                            view.message = if view.browser.english {
                                format!(
                                    "{} · {count} tracks",
                                    if already {
                                        "Playlist already saved"
                                    } else {
                                        "Playlist saved"
                                    }
                                )
                            } else {
                                format!(
                                    "{} · {count} 首",
                                    if already {
                                        "已收藏该歌单"
                                    } else {
                                        "已收藏歌单"
                                    }
                                )
                            };
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
                if view.sync_covers(&actions) {
                    dirty = true;
                }
                if dirty {
                    terminal.draw(|frame| draw(frame, &mut view))?;
                    preview_renderer.draw(
                        view.preview.area,
                        view.preview.key(),
                        view.preview.pixels(),
                    )?;
                    playing_renderer.draw(
                        view.playing_cover.area,
                        view.playing_cover.key(),
                        view.playing_cover.pixels(),
                    )?;
                    dirty = false;
                }
                if !event::poll(Duration::from_millis(80))? {
                    continue;
                }
                let key = match event::read()? {
                    Event::Key(key) if key.is_press() => key,
                    Event::Resize(_, _) => {
                        preview_renderer.draw(None, None, None)?;
                        playing_renderer.draw(None, None, None)?;
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
                            let search_page = if view.page == 1 { 1 } else { 0 };
                            if search_page == 1 {
                                while view.browser.back(1) {}
                            }
                            view.page = search_page;
                            view.focus = 1;
                            view.browser.routes[search_page].query = query;
                            view.refresh(&actions, false);
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            input.clear()
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
                        let search_page = if view.page == 1 { 1 } else { 0 };
                        view.browser.input = Some(view.browser.routes[search_page].query.clone());
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
                    KeyCode::Char('F' | 'f')
                        if key.code == KeyCode::Char('F')
                            || key.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        if let Some(command) = view.browser.playlist_favorite(view.page) {
                            view.send(&actions, Action::Service(command));
                        } else {
                            view.message = if view.browser.playlist_play(view.page).is_some() {
                                if view.browser.english {
                                    "This playlist is already in My Library"
                                } else {
                                    "该歌单已在我的列表中"
                                }
                            } else if view.browser.english {
                                "Select or open a playlist first"
                            } else {
                                "请先选中或打开一个歌单"
                            }
                            .into();
                        }
                    }
                    KeyCode::Char('f') => {
                        if let Some(track) = view.browser.track_id(view.page) {
                            view.send(
                                &actions,
                                Action::Service(ServiceCommand::Favorite { track }),
                            );
                        } else if let Some(command) = view.browser.playlist_favorite(view.page) {
                            view.send(&actions, Action::Service(command));
                        }
                    }
                    KeyCode::Char('b') => {
                        command = view.browser.playlist_play(view.page);
                        if command.is_some() {
                            view.focus = 1;
                        } else {
                            view.message = if view.browser.english {
                                "Select or open a playlist first"
                            } else {
                                "请先选中或打开一个歌单"
                            }
                            .into();
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
    cover_supported: bool,
    preview: cover::State,
    playing_cover: cover::State,
}
impl View {
    fn sync_covers(&mut self, actions: &mpsc::Sender<Action>) -> bool {
        if !self.cover_supported {
            return false;
        }
        let current = self
            .snapshot
            .current_entry()
            .map(|entry| cover::Target::track(entry.track.clone()));
        let selected = match self.focus {
            1 if !self.lyrics_mode => self.browser.cover_target(self.page),
            2 => self
                .queue
                .selected()
                .and_then(|index| self.snapshot.queue.get(index))
                .map(|entry| cover::Target::track(entry.track.clone())),
            _ => None,
        };
        let mut changed = self.playing_cover.set(current.clone(), Duration::ZERO);
        changed |= self
            .preview
            .set(selected.or(current), Duration::from_millis(200));
        for (slot, state) in [
            (cover::Slot::Playing, &mut self.playing_cover),
            (cover::Slot::Preview, &mut self.preview),
        ] {
            if let Some(target) = state.request() {
                if actions
                    .try_send(Action::Cover {
                        slot,
                        target: Box::new(target),
                    })
                    .is_err()
                {
                    state.retry();
                }
                changed = true;
            }
        }
        changed
    }
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
    view.preview.area = None;
    view.playing_cover.area = None;
    let english = view.browser.english;
    let theme = Theme::named(&view.config.theme);
    frame.render_widget(Block::new().style(theme.base()), frame.area());
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(2),
        Constraint::Length(
            if view.cover_supported && frame.area().width >= 64 && frame.area().height >= 24 {
                6
            } else {
                3
            },
        ),
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
        .style(
            theme
                .base()
                .fg(theme.secondary)
                .bg(theme.panel)
                .add_modifier(Modifier::BOLD),
        )
        .block(theme.block("", false).style(theme.base().bg(theme.panel))),
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
            .style(theme.base().bg(theme.panel).fg(theme.muted))
            .block(
                theme
                    .block(if english { "Browse" } else { "浏览" }, view.focus == 0)
                    .style(theme.base().bg(theme.panel)),
            )
            .highlight_style(theme.selected(view.focus == 0))
            .highlight_symbol(if view.focus == 0 { "› " } else { "  " }),
        columns[0],
        &mut ListState::default().with_selected(Some(view.page)),
    );
    let (mut content, queue) = if frame.area().width >= 100 {
        let split =
            Layout::horizontal([Constraint::Min(20), Constraint::Length(30)]).split(columns[1]);
        (Some(split[0]), Some(split[1]))
    } else if view.focus == 2 {
        (None, Some(columns[1]))
    } else {
        (Some(columns[1]), None)
    };
    if queue.is_none()
        && view.cover_supported
        && let Some(area) = content
        && area.width >= 44
        && area.height >= 12
    {
        let split = Layout::horizontal([Constraint::Min(20), Constraint::Length(24)]).split(area);
        content = Some(split[0]);
        draw_cover(
            frame,
            view,
            Rect::new(split[1].x, split[1].y, split[1].width, area.height.min(14)),
        );
    }
    if let Some(area) = content {
        if view.lyrics_mode {
            if let Some(lyrics) = &view.lyrics {
                if lyrics.lines.is_empty() {
                    frame.render_widget(
                        Paragraph::new(lyrics.plain.clone())
                            .style(theme.base())
                            .block(
                                theme.block(
                                    if english { "Lyrics" } else { "歌词" },
                                    view.focus == 1,
                                ),
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
                            .style(theme.subdued())
                            .block(
                                theme.block(
                                    if english { "Lyrics" } else { "歌词" },
                                    view.focus == 1,
                                ),
                            )
                            .highlight_style(theme.selected(true).fg(theme.success)),
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
                    .style(theme.subdued())
                    .block(theme.block(if english { "Lyrics" } else { "歌词" }, view.focus == 1)),
                    area,
                );
            }
        } else {
            view.browser
                .draw(frame, area, view.page, view.focus == 1, theme);
        }
    }
    if let Some(mut area) = queue {
        if view.cover_supported && area.width >= 24 && area.height >= 14 {
            let split = Layout::vertical([
                Constraint::Length(area.height.saturating_sub(4).min(14)),
                Constraint::Min(4),
            ])
            .split(area);
            draw_cover(frame, view, split[0]);
            area = split[1];
        }
        let items = view.snapshot.queue.iter().map(|e| {
            let current = view.snapshot.current == Some(e.id);
            ListItem::new(Line::from(vec![
                Span::styled(
                    if current { "▶ " } else { "  " },
                    Style::default().fg(theme.success),
                ),
                Span::styled(
                    e.track.title.clone(),
                    Style::default().fg(if current { theme.success } else { theme.text }),
                ),
            ]))
        });
        frame.render_stateful_widget(
            List::new(items)
                .style(theme.base())
                .block(theme.block(if english { "Queue" } else { "队列" }, view.focus == 2))
                .highlight_style(theme.selected(view.focus == 2))
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
    let state_color = match view.snapshot.state {
        PlaybackState::Playing => theme.success,
        PlaybackState::Paused => theme.warning,
        PlaybackState::Resolving | PlaybackState::Buffering => theme.secondary,
        PlaybackState::Error => theme.error,
        _ => theme.muted,
    };
    let status = Line::from(vec![
        Span::styled(
            format!(" {title}"),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | ", Style::default().fg(theme.border)),
        Span::styled(state, Style::default().fg(state_color)),
        Span::styled(
            format!(" {:.0}s / {duration}", view.snapshot.position),
            Style::default().fg(theme.muted),
        ),
        Span::styled(" | ", Style::default().fg(theme.border)),
        Span::styled(
            format!(
                "{} {}%",
                if english { "Volume" } else { "音量" },
                view.snapshot.volume
            ),
            Style::default().fg(theme.secondary),
        ),
        Span::styled(format!(" · {mode}"), Style::default().fg(theme.accent)),
    ]);
    if rows[2].height >= 6 {
        let block = theme
            .block(
                if english {
                    "Now playing"
                } else {
                    "正在播放"
                },
                false,
            )
            .style(theme.base().bg(theme.panel));
        let inner = block.inner(rows[2]);
        frame.render_widget(block, rows[2]);
        let image = Rect::new(inner.x, inner.y, 8.min(inner.width), inner.height.min(4));
        if view.playing_cover.png.is_some() {
            view.playing_cover.area = Some(image);
        } else {
            frame.render_widget(
                Paragraph::new("♪")
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(theme.subdued().bg(theme.panel)),
                Rect::new(image.x, image.y + image.height / 2, image.width, 1),
            );
        }
        let info = Rect::new(
            inner.x + 10.min(inner.width),
            inner.y,
            inner.width.saturating_sub(10),
            inner.height,
        );
        let artist = view
            .snapshot
            .current_entry()
            .map(|entry| {
                format!(
                    "{}{}{}",
                    entry.track.artist,
                    if entry.track.artist.is_empty() || entry.track.album.is_empty() {
                        ""
                    } else {
                        " · "
                    },
                    entry.track.album
                )
            })
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    title,
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Line::styled(artist, Style::default().fg(theme.muted)),
                Line::from(vec![
                    Span::styled(state, Style::default().fg(state_color)),
                    Span::styled(
                        format!(
                            "  {:.0}s / {duration}  |  {} {}%  ·  {mode}",
                            view.snapshot.position,
                            if english { "Volume" } else { "音量" },
                            view.snapshot.volume
                        ),
                        Style::default().fg(theme.secondary),
                    ),
                ]),
            ])
            .style(theme.base().bg(theme.panel)),
            info,
        );
    } else {
        frame.render_widget(
            Paragraph::new(status)
                .style(theme.base().bg(theme.panel))
                .block(
                    theme
                        .block(if english { "Player" } else { "播放" }, false)
                        .style(theme.base().bg(theme.panel)),
                ),
            rows[2],
        );
    }
    if let Some(input) = &view.browser.input {
        frame.render_widget(
            Paragraph::new(format!(" / {input}"))
                .style(theme.base())
                .block(
                    theme
                        .block(
                            if english {
                                "Search · Enter to submit · Esc to cancel"
                            } else {
                                "搜索 · Enter 提交 · Esc 取消"
                            },
                            true,
                        )
                        .border_style(Style::default().fg(theme.secondary)),
                ),
            rows[3],
        );
    } else {
        let keys = if english {
            " q: leave  b: play playlist  F: save playlist  /: search  Tab: focus  Enter: open/play  ]: more  a: queue  f: favorite"
        } else {
            " q 退出  b 整单播放  F 整单收藏  / 搜索  Tab 焦点  Enter 打开/播放  ] 更多  a 入队  f 收藏"
        };
        let message = view.snapshot.error.as_deref().unwrap_or(&view.message);
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(keys, Style::default().fg(theme.muted)),
                Line::styled(
                    format!(" {message}"),
                    Style::default().fg(if view.snapshot.error.is_some() {
                        theme.error
                    } else {
                        theme.secondary
                    }),
                ),
            ])
            .style(theme.base()),
            rows[3],
        );
    }
}

fn draw_cover(frame: &mut Frame, view: &mut View, cover: Rect) {
    let theme = Theme::named(&view.config.theme);
    let block = theme.block(
        if view.browser.english {
            "Preview"
        } else {
            "预览"
        },
        false,
    );
    let inner = block.inner(cover);
    frame.render_widget(block, cover);
    let height = inner.height.min(inner.width / 2).max(1);
    let width = inner.width.min(height * 2);
    let image = Rect::new(
        inner.x + (inner.width - width) / 2,
        inner.y + inner.height.saturating_sub(height) / 2,
        width,
        height,
    );
    if view.preview.png.is_some() {
        view.preview.area = Some(image);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{QueueEntry, Track};
    use uuid::Uuid;

    #[test]
    fn lyrics_queue_navigation_updates_preview_without_changing_playing_cover() {
        let queue = (1..=2)
            .map(|number| QueueEntry {
                id: Uuid::new_v4(),
                track: serde_json::from_value::<Track>(serde_json::json!({
                    "id": format!("wy:{number}"),
                    "title": format!("Song {number}"),
                    "artist": "Fixture",
                    "artwork": format!("https://example.test/{number}.png")
                }))
                .unwrap(),
            })
            .collect::<Vec<_>>();
        let playing = cover::Target::track(queue[0].track.clone()).key;
        let selected = cover::Target::track(queue[1].track.clone()).key;
        let config = Config::default();
        let mut view = View {
            lyrics_mode: true,
            lyric_track: None,
            lyrics: None,
            snapshot: Snapshot {
                current: Some(queue[0].id),
                queue,
                ..Snapshot::default()
            },
            page: 1,
            focus: 2,
            queue: ListState::default().with_selected(Some(0)),
            message: String::new(),
            browser: Browser::new(config.platform, false),
            config,
            cover_supported: true,
            preview: cover::State::default(),
            playing_cover: cover::State::default(),
        };
        let (actions, _updates) = mpsc::channel(8);
        view.sync_covers(&actions);
        assert_eq!(view.preview.key(), Some(playing.as_str()));
        view.navigate(1, &actions);
        assert_eq!(view.queue.selected(), Some(1));
        view.sync_covers(&actions);
        assert_eq!(
            view.preview.key(),
            Some(selected.as_str()),
            "Lyrics view must preview the selected queue track"
        );
        assert_eq!(view.playing_cover.key(), Some(playing.as_str()));

        view.focus = 1;
        view.sync_covers(&actions);
        assert_eq!(
            view.preview.key(),
            Some(playing.as_str()),
            "Lyrics focus should follow actual playback"
        );
        view.snapshot.current = Some(view.snapshot.queue[1].id);
        view.sync_covers(&actions);
        assert_eq!(view.preview.key(), Some(selected.as_str()));
        assert_eq!(view.playing_cover.key(), Some(selected.as_str()));
        view.focus = 2;
        view.navigate(-1, &actions);
        view.sync_covers(&actions);
        assert_eq!(view.preview.key(), Some(playing.as_str()));
        assert_eq!(view.playing_cover.key(), Some(selected.as_str()));
    }
}
