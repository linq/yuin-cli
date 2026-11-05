use crate::{domain::Platform, services::ServiceCommand};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone)]
pub enum Kind {
    Search,
    Collections { charts: bool },
    Tracks { id: String, chart: bool },
    Library,
    LibraryTracks { id: Uuid },
    Downloads,
}
#[derive(Clone)]
pub struct Content {
    pub kind: Kind,
    pub title: String,
    pub items: Vec<Value>,
    pub list: ListState,
    pub page: u32,
    pub has_more: bool,
    pub total: Option<usize>,
    pub loaded: bool,
}
pub struct Route {
    pub content: Content,
    pub history: Vec<Content>,
    pub query: String,
    pub version: u64,
    pub loading: bool,
    pub append: bool,
}
pub struct Browser {
    pub routes: Vec<Route>,
    pub platform: Platform,
    pub input: Option<String>,
    pub english: bool,
}
impl Browser {
    pub fn new(platform: Platform, english: bool) -> Self {
        let definitions = [
            (Kind::Search, "Search", "搜索"),
            (Kind::Collections { charts: false }, "Playlists", "歌单"),
            (Kind::Collections { charts: true }, "Charts", "排行榜"),
            (Kind::Library, "My Library", "我的列表"),
            (Kind::Downloads, "Downloads", "下载"),
        ];
        let routes = definitions
            .into_iter()
            .map(|(kind, en, zh)| Route {
                content: Content {
                    kind,
                    title: if english { en } else { zh }.into(),
                    items: Vec::new(),
                    list: ListState::default(),
                    page: 1,
                    has_more: false,
                    total: None,
                    loaded: false,
                },
                history: Vec::new(),
                query: String::new(),
                version: u64::from_be_bytes(
                    Uuid::new_v4().as_bytes()[..8]
                        .try_into()
                        .expect("UUID prefix"),
                ) & ((1_u64 << 62) - 1),
                loading: false,
                append: false,
            })
            .collect();
        Self {
            routes,
            platform,
            input: None,
            english,
        }
    }
    pub fn query(&mut self, index: usize, append: bool) -> Option<(u64, ServiceCommand)> {
        let route = &mut self.routes[index];
        let page = if append { route.content.page + 1 } else { 1 };
        let command = match &route.content.kind {
            Kind::Search => {
                if route.query.trim().is_empty() {
                    return None;
                }
                ServiceCommand::Search {
                    query: route.query.clone(),
                    platform: Some(self.platform),
                    page,
                }
            }
            Kind::Collections { charts } => ServiceCommand::Collections {
                platform: Some(self.platform),
                charts: *charts,
                page,
            },
            Kind::Tracks { id, chart } => ServiceCommand::CollectionTracks {
                id: id.clone(),
                chart: *chart,
                page,
                all: false,
            },
            Kind::Library => ServiceCommand::LibraryList,
            Kind::LibraryTracks { id } => ServiceCommand::LibraryShow { id: *id, page },
            Kind::Downloads => ServiceCommand::Downloads { page },
        };
        route.loading = true;
        route.append = append;
        route.version += 1;
        Some((route.version, command))
    }
    pub fn apply(&mut self, index: usize, version: u64, value: Value) -> bool {
        let route = &mut self.routes[index];
        if route.version != version {
            return false;
        }
        route.loading = false;
        let selected = route
            .content
            .list
            .selected()
            .and_then(|i| route.content.items.get(i))
            .and_then(|v| v["id"].as_str())
            .map(str::to_string);
        let items = value["items"]
            .as_array()
            .or_else(|| value["playlist"]["tracks"].as_array())
            .cloned()
            .unwrap_or_default();
        if route.append {
            let mut seen: HashSet<String> = route
                .content
                .items
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            route.content.items.extend(
                items
                    .into_iter()
                    .filter(|v| v["id"].as_str().is_none_or(|id| seen.insert(id.into()))),
            );
        } else {
            route.content.items = items;
        }
        route.content.page = value["page"].as_u64().unwrap_or(1) as u32;
        route.content.has_more = value["has_more"] == true;
        route.content.total = value["total"].as_u64().map(|n| n as usize);
        route.content.loaded = true;
        let index = selected
            .and_then(|id| route.content.items.iter().position(|v| v["id"] == id))
            .or({
                if route.content.items.is_empty() {
                    None
                } else {
                    Some(0)
                }
            });
        route.content.list.select(index);
        true
    }
    pub fn navigate(&mut self, index: usize, delta: isize) {
        let route = &mut self.routes[index];
        if !route.content.items.is_empty() {
            let current = route
                .content
                .list
                .selected()
                .unwrap_or(0)
                .saturating_add_signed(delta)
                .min(route.content.items.len() - 1);
            route.content.list.select(Some(current));
        }
    }
    pub fn selected(&self, index: usize) -> Option<&Value> {
        let route = &self.routes[index];
        route
            .content
            .list
            .selected()
            .and_then(|i| route.content.items.get(i))
    }
    pub fn track_id(&self, index: usize) -> Option<String> {
        match self.routes[index].content.kind {
            Kind::Search | Kind::Tracks { .. } | Kind::LibraryTracks { .. } => self
                .selected(index)?
                .get("id")?
                .as_str()
                .map(str::to_string),
            _ => None,
        }
    }
    pub fn enter(&mut self, index: usize) -> bool {
        let Some(value) = self.selected(index).cloned() else {
            return false;
        };
        let kind = match &self.routes[index].content.kind {
            Kind::Collections { charts } => Kind::Tracks {
                id: value["id"].as_str().unwrap_or_default().into(),
                chart: *charts,
            },
            Kind::Library => {
                let Some(id) = value["id"].as_str().and_then(|s| s.parse().ok()) else {
                    return false;
                };
                Kind::LibraryTracks { id }
            }
            _ => return false,
        };
        let route = &mut self.routes[index];
        let title = value["title"]
            .as_str()
            .or(value["name"].as_str())
            .unwrap_or_default()
            .to_string();
        let content = Content {
            kind,
            title,
            items: Vec::new(),
            list: ListState::default(),
            page: 1,
            has_more: false,
            total: None,
            loaded: false,
        };
        route
            .history
            .push(std::mem::replace(&mut route.content, content));
        true
    }
    pub fn back(&mut self, index: usize) -> bool {
        let route = &mut self.routes[index];
        if let Some(previous) = route.history.pop() {
            route.version += 1;
            route.loading = false;
            route.content = previous;
            true
        } else {
            false
        }
    }
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        index: usize,
        focus: bool,
        accent: Color,
    ) {
        let route = &mut self.routes[index];
        let page = &mut route.content;
        let title = format!(
            " {}{} · {}{}{} ",
            page.title,
            if focus { " •" } else { "" },
            page.items.len(),
            page.total.map(|n| format!("/{n}")).unwrap_or_default(),
            if route.loading { " …" } else { "" }
        );
        if page.items.is_empty() {
            let text = if route.loading {
                if self.english {
                    "Loading…"
                } else {
                    "加载中…"
                }
            } else if matches!(page.kind, Kind::Search) {
                if self.english {
                    "Press / to search.\nP: platform  r: refresh"
                } else {
                    "按 / 搜索。\nP：切换平台  r：刷新"
                }
            } else {
                if self.english {
                    "No items. r: refresh"
                } else {
                    "暂无内容。r：刷新"
                }
            };
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(title)),
                area,
            );
            return;
        }
        let items = page.items.iter().map(|v| {
            let title = v["title"]
                .as_str()
                .or(v["name"].as_str())
                .or(v["track"]["title"].as_str())
                .unwrap_or("—");
            let artist = v["artist"]
                .as_str()
                .or(v["subtitle"].as_str())
                .unwrap_or_default();
            let state = v["state"].as_str().unwrap_or_default();
            let extra = if !state.is_empty() {
                format!(" [{state}] {}", v["quality"].as_str().unwrap_or_default())
            } else if let Some(count) = v["count"].as_u64() {
                format!(" ({count})")
            } else {
                String::new()
            };
            ListItem::new(format!("{title}  {artist}{extra}"))
        });
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(title))
                .highlight_style(Style::default().fg(accent))
                .highlight_symbol("› "),
            area,
            &mut page.list,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn stale_queries_are_ignored_and_append_and_back_preserve_selection_and_scroll() {
        let mut browser = Browser::new(Platform::Wy, true);
        let (old, _) = browser.query(1, false).unwrap();
        let (current, _) = browser.query(1, false).unwrap();
        assert!(!browser.apply(1, old, json!({"items":[{"id":"wrong"}]})));
        assert!(browser.apply(1,current,json!({"items":[{"id":"wy:1","title":"first"},{"id":"wy:2","title":"second"}],"page":1,"total":3,"has_more":true})));
        browser.routes[1].content.list.select(Some(1));
        *browser.routes[1].content.list.offset_mut() = 1;
        let (append, _) = browser.query(1, true).unwrap();
        browser.apply(
            1,
            append,
            json!({"items":[{"id":"wy:3","title":"third"}],"page":2,"total":3,"has_more":false}),
        );
        assert_eq!(browser.routes[1].content.list.selected(), Some(1));
        assert_eq!(browser.routes[1].content.list.offset(), 1);
        assert!(browser.enter(1));
        assert!(browser.back(1));
        assert_eq!(browser.selected(1).unwrap()["id"], "wy:2");
        assert_eq!(browser.routes[1].content.list.offset(), 1);
    }
}
