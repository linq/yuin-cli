//! Public metadata adapters, ported from Yuin's reference desktop implementation.
//! Endpoint/signature provenance is recorded in docs/THIRD_PARTY_NOTICES.md.
mod parse;
use crate::domain::{Collection, CollectionPage, Platform, Track};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use parse::{array, number, string, track};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone)]
pub struct Catalog {
    client: reqwest::Client,
}

impl Catalog {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: crate::http::client()?,
        })
    }

    async fn request(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
        body: Option<Value>,
        signed: bool,
        headers: &[(&str, String)],
    ) -> Result<Value> {
        let mut url = crate::http::url(endpoint)?;
        let referer = format!("{}://{}/", url.scheme(), url.host_str().unwrap_or_default());
        if let Ok(base) = std::env::var("YUIN_CATALOG_BASE") {
            let original = url.clone();
            url = crate::http::url(&base)?.join(original.path())?;
        }
        let bytes = body.as_ref().map(serde_json::to_vec).transpose()?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v)));
        if signed {
            let digest = Sha1::digest(bytes.as_deref().unwrap_or_default());
            let hex = digest
                .iter()
                .map(|n| format!("{n:02x}"))
                .collect::<String>();
            let pick = |indexes: &[usize]| {
                indexes
                    .iter()
                    .filter_map(|i| hex.as_bytes().get(*i))
                    .map(|b| *b as char)
                    .collect::<String>()
            };
            let mask = [
                89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161,
                121, 179,
            ];
            let middle = STANDARD
                .encode(
                    digest
                        .iter()
                        .zip(mask)
                        .map(|(a, b)| a ^ b)
                        .collect::<Vec<_>>(),
                )
                .replace(['/', '+', '=', '\\'], "");
            let sign = format!(
                "zzc{}{middle}{}",
                pick(&[23, 14, 6, 36, 16, 40, 7, 19]),
                pick(&[16, 1, 32, 12, 19, 27, 8, 5])
            )
            .to_lowercase();
            url.query_pairs_mut().append_pair("sign", &sign);
        }
        let mut request = if let Some(bytes) = bytes {
            self.client
                .post(url)
                .header("Content-Type", "application/json")
                .body(bytes)
        } else {
            self.client.get(url)
        };
        request = request.header("Referer", referer);
        if signed {
            request = request.header("User-Agent", "QQMusic 14090508(android 12)");
        }
        for (key, value) in headers {
            request = request.header(*key, value);
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Platform network request failed"))?;
        let bytes = crate::http::bytes(response, 30_000_000).await?;
        let text = String::from_utf8_lossy(&bytes);
        serde_json::from_str(&text).context("Platform returned invalid JSON")
    }

    pub async fn search(
        &self,
        query: &str,
        platform: Platform,
        page: u32,
    ) -> Result<CollectionPage<Track>> {
        ensure!(page > 0 && page <= 1000, "Page must be between 1 and 1000");
        ensure!(
            !query.trim().is_empty() && query.len() <= 1024,
            "Search requires 1–1024 bytes"
        );
        let size = 30;
        let (rows, total) = match platform {
            Platform::Wy => {
                let d = self
                    .request(
                        "https://music.163.com/api/search/get",
                        &[
                            ("s", query.into()),
                            ("type", "1".into()),
                            ("limit", size.to_string()),
                            ("offset", ((page - 1) * size).to_string()),
                        ],
                        None,
                        false,
                        &[],
                    )
                    .await?;
                ensure!(
                    d["code"] == 200 && d["result"].is_object(),
                    "NetEase search is unavailable"
                );
                (
                    array(&d["result"], &["songs"]),
                    number(&d["result"], &["songCount"]) as usize,
                )
            }
            Platform::Kw => {
                let d = self
                    .request(
                        "https://search.kuwo.cn/r.s",
                        &[
                            ("client", "kt".into()),
                            ("all", query.into()),
                            ("pn", (page - 1).to_string()),
                            ("rn", size.to_string()),
                            ("ft", "music".into()),
                            ("encoding", "utf8".into()),
                            ("rformat", "json".into()),
                            ("vipver", "1".into()),
                            ("newver", "1".into()),
                            ("mobi", "1".into()),
                            ("vermerge", "1".into()),
                            ("show_copyright_off", "1".into()),
                        ],
                        None,
                        false,
                        &[],
                    )
                    .await?;
                ensure!(!d["TOTAL"].is_null(), "Kuwo search is unavailable");
                (array(&d, &["abslist"]), number(&d, &["TOTAL"]) as usize)
            }
            Platform::Kg => {
                let d = self
                    .request(
                        "https://songsearch.kugou.com/song_search_v2",
                        &[
                            ("platform", "AndroidFilter".into()),
                            ("iscorrection", "1".into()),
                            ("keyword", query.into()),
                            ("hifiquality", "0".into()),
                            ("pagesize", size.to_string()),
                            ("PrivilegeFilter", "0".into()),
                            ("page", page.to_string()),
                        ],
                        None,
                        false,
                        &[],
                    )
                    .await?;
                ensure!(d["status"] == 1, "Kugou search is unavailable");
                (
                    array(&d["data"], &["lists"]),
                    number(&d["data"], &["total"]) as usize,
                )
            }
            Platform::Tx => {
                let body = json!({"comm":{"ct":"19","cv":"2151","uin":"0","tmeAppID":"qqmusic","tmeLoginType":0},"req":{"module":"music.search.SearchCgiService","method":"DoSearchForQQMusicDesktop","param":{"query":query,"num_per_page":size,"page_num":page,"search_type":0,"grp":1,"remoteplace":"txt.newclient.top","searchid":format!("{}00000",Uuid::new_v4().simple())}}});
                let d = self
                    .request(
                        "https://u.y.qq.com/cgi-bin/musics.fcg",
                        &[],
                        Some(body),
                        true,
                        &[],
                    )
                    .await?;
                ensure!(
                    d["code"] == 0
                        && d["req"]["code"] == 0
                        && d["req"]["data"]["body"]["song"]["list"].is_array(),
                    "QQ search is unavailable"
                );
                (
                    array(&d["req"]["data"]["body"]["song"], &["list"]),
                    number(&d["req"]["data"]["meta"], &["sum"]) as usize,
                )
            }
            Platform::Mg => {
                use md5::Md5;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis()
                    .to_string();
                let device = "963B7AA0D21511ED807EE5846EC87D20";
                let sign = Md5::digest(format!("{query}6cdc72a439cef99a3418d2a78aa28c73yyapp2d16148780a1dcc7408e06336b98cfd50{device}{stamp}").as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>();
                let d = self.request("https://jadeite.migu.cn/music_search/v3/search/searchAll", &[("text",query.into()),("pageNo",page.to_string()),("pageSize",size.to_string()),("searchSwitch",r#"{"song":1,"album":0,"singer":0,"tagSong":1,"mvSong":0,"bestShow":1,"songlist":0,"lyricSong":0}"#.into()),("sid","USS".into()),("isCopyright","1".into()),("isCorrect","0".into()),("sort","0".into())],None,false,&[("uiVersion","A_music_3.6.1".into()),("deviceId",device.into()),("timestamp",stamp),("sign",sign),("channel","0146921".into())]).await?;
                ensure!(
                    d["code"] == "000000" && d["songResultData"].is_object(),
                    "Migu search is unavailable"
                );
                let rows = array(&d["songResultData"], &["resultList"])
                    .iter()
                    .flat_map(|r| r.as_array().cloned().unwrap_or_else(|| vec![r.clone()]))
                    .collect();
                (rows, number(&d["songResultData"], &["totalCount"]) as usize)
            }
        };
        let mut seen = HashSet::new();
        let items = rows
            .iter()
            .filter_map(|r| track(r, platform))
            .filter(|t| seen.insert(t.id.clone()))
            .collect();
        Ok(CollectionPage::new(items, total, page, size as usize))
    }

    pub async fn detail(&self, id: &str) -> Result<Track> {
        let (platform, song_id) = parse_id(id)?;
        ensure!(
            platform == Platform::Wy,
            "Track metadata is not cached; search or open its platform collection first"
        );
        let songs = self.netease_details(&[song_id]).await?;
        songs
            .into_iter()
            .find(|t| t.id == id)
            .context("Track is unavailable")
    }

    async fn netease_details(&self, ids: &[String]) -> Result<Vec<Track>> {
        let numbers: Vec<u64> = ids.iter().map(|id| id.parse()).collect::<Result<_, _>>()?;
        let d = self
            .request(
                "https://music.163.com/api/v3/song/detail",
                &[
                    (
                        "c",
                        serde_json::to_string(
                            &numbers
                                .iter()
                                .map(|id| json!({"id":id}))
                                .collect::<Vec<_>>(),
                        )?,
                    ),
                    ("ids", serde_json::to_string(&numbers)?),
                ],
                None,
                false,
                &[],
            )
            .await?;
        ensure!(
            d["code"] == 200 && d["songs"].is_array(),
            "NetEase detail is unavailable"
        );
        Ok(array(&d, &["songs"])
            .iter()
            .filter_map(|r| track(r, Platform::Wy))
            .collect())
    }

    pub async fn collections(
        &self,
        platform: Platform,
        charts: bool,
        page: u32,
    ) -> Result<CollectionPage<Collection>> {
        ensure!(page > 0 && page <= 1000, "Invalid page");
        let rows: Vec<Value>;
        let total: usize;
        let size;
        if charts {
            ensure!(page == 1, "Chart index has only one page");
            rows = match platform {
                Platform::Wy => {
                    let d = self
                        .request("https://music.163.com/api/toplist", &[], None, false, &[])
                        .await?;
                    ensure!(
                        d["code"] == 200 && d["list"].is_array(),
                        "NetEase chart index unavailable"
                    );
                    array(&d, &["list"])
                }
                Platform::Kw => [
                    ("93", "飙升榜"),
                    ("16", "热歌榜"),
                    ("17", "新歌榜"),
                    ("26", "经典怀旧榜"),
                    ("104", "华语榜"),
                    ("22", "欧美榜"),
                ]
                .iter()
                .map(|(id, name)| json!({"id":id,"name":name}))
                .collect(),
                Platform::Kg => {
                    let d = self
                        .request(
                            "http://mobilecdnbj.kugou.com/api/v5/rank/list",
                            &[
                                ("version", "9108".into()),
                                ("plat", "0".into()),
                                ("showtype", "2".into()),
                                ("parentid", "0".into()),
                                ("apiver", "6".into()),
                                ("area_code", "1".into()),
                                ("withsong", "1".into()),
                            ],
                            None,
                            false,
                            &[],
                        )
                        .await?;
                    ensure!(d["status"] == 1, "Kugou chart index unavailable");
                    array(&d["data"], &["info"])
                }
                Platform::Tx => {
                    let d = self.request("https://u.y.qq.com/cgi-bin/musicu.fcg",&[],Some(json!({"req":{"module":"musicToplist.ToplistInfoServer","method":"GetAll","param":{}}})),false,&[]).await?;
                    ensure!(d["req"]["code"] == 0, "QQ chart index unavailable");
                    array(&d["req"]["data"], &["group"])
                        .iter()
                        .flat_map(|g| array(g, &["toplist"]))
                        .collect()
                }
                Platform::Mg => {
                    let d = self
                        .request(
                            "https://app.c.nf.migu.cn/pc/bmw/rank/rank-index/v1.0",
                            &[],
                            None,
                            false,
                            &[("channel", "0146921".into())],
                        )
                        .await?;
                    ensure!(d["code"] == "000000", "Migu chart index unavailable");
                    flatten(&d["data"]["contents"], "rankId")
                }
            };
            total = rows.len();
            size = total.max(1);
        } else {
            size = if platform == Platform::Kg { 20 } else { 30 };
            let d = match platform {
                Platform::Wy => self.request("https://music.163.com/api/playlist/list", &[("cat","全部".into()),("order","hot".into()),("limit","30".into()),("offset",((page-1)*30).to_string())],None,false,&[]).await?,
                Platform::Kw => self.request("https://wapi.kuwo.cn/api/pc/classify/playlist/getRcmPlayList", &[("loginUid","0".into()),("loginSid","0".into()),("appUid","76039576".into()),("pn",page.to_string()),("rn","30".into()),("order","hot".into())],None,false,&[]).await?,
                Platform::Kg => self.request("http://www2.kugou.kugou.com/yueku/v9/special/getSpecial", &[("is_ajax","1".into()),("cdn","cdn".into()),("t","5".into()),("c","".into()),("p",page.to_string())],None,false,&[]).await?,
                Platform::Tx => self.request("https://u.y.qq.com/cgi-bin/musicu.fcg", &[],Some(json!({"comm":{"cv":1602,"ct":20},"req":{"module":"music.playlist.PlaylistSquare","method":"GetRecommendWhole","param":{"IsReqFeed":true,"FeedReq":{"From":(page-1)*30,"Size":30}}}})),false,&[]).await?,
                Platform::Mg => self.request("https://app.c.nf.migu.cn/pc/bmw/page-data/playlist-square-recommend/v1.0", &[("templateVersion","2".into()),("pageNo",page.to_string())],None,false,&[]).await?,
            };
            rows = match platform {
                Platform::Wy => {
                    ensure!(
                        d["code"] == 200 && d["playlists"].is_array(),
                        "NetEase playlists unavailable"
                    );
                    array(&d, &["playlists"])
                }
                Platform::Kw => {
                    ensure!(d["code"] == 200, "Kuwo playlists unavailable");
                    array(&d["data"], &["data"])
                        .into_iter()
                        .filter(|r| string(r, &["digest"]) == "8")
                        .collect()
                }
                Platform::Kg => {
                    ensure!(
                        d["status"] == 1 && d["special_db"].is_array(),
                        "Kugou playlists unavailable"
                    );
                    array(&d, &["special_db"])
                }
                Platform::Tx => {
                    ensure!(d["req"]["code"] == 0, "QQ playlists unavailable");
                    array(&d["req"]["data"]["FeedRsp"], &["List"])
                        .iter()
                        .map(|r| r["Playlist"]["basic"].clone())
                        .collect()
                }
                Platform::Mg => {
                    ensure!(d["code"] == "000000", "Migu playlists unavailable");
                    flatten(&d["data"]["contents"], "resId")
                        .into_iter()
                        .filter(|r| string(r, &["resType"]) == "2021")
                        .collect()
                }
            };
            total = number(&d, &["total"]) as usize;
        }
        let mut seen = HashSet::new();
        let items = rows
            .iter()
            .filter_map(|r| parse::collection(r, platform, charts))
            .filter(|r| seen.insert(r.id.clone()))
            .collect::<Vec<_>>();
        let len = items.len();
        let mut result = CollectionPage::new(items, total.max(len), page, size);
        if !charts && total == 0 {
            result.total = None;
            result.has_more = len >= size;
        }
        Ok(result)
    }

    pub async fn tracks(&self, id: &str, chart: bool, page: u32) -> Result<CollectionPage<Track>> {
        ensure!(page > 0 && page <= 1000, "Invalid page");
        let (platform, collection) = parse_id(id)?;
        let size = if platform == Platform::Mg && !chart {
            50
        } else {
            100
        };
        if platform == Platform::Wy {
            let d = self
                .request(
                    "https://music.163.com/api/v3/playlist/detail",
                    &[("id", collection), ("n", "10000".into())],
                    None,
                    false,
                    &[],
                )
                .await?;
            let list = &d["playlist"];
            ensure!(
                d["code"] == 200 && list["trackIds"].is_array(),
                "NetEase playlist unavailable"
            );
            let ids = array(list, &["trackIds"])
                .iter()
                .map(|r| string(r, &["id"]))
                .collect::<Vec<_>>();
            let total = (number(list, &["trackCount"]) as usize).max(ids.len());
            let unique: HashSet<_> = ids.iter().collect();
            ensure!(
                ids.len() == total && unique.len() == ids.len(),
                "Incomplete playlist: ordered IDs do not match track count"
            );
            let wanted = ids
                .into_iter()
                .skip((page as usize - 1) * size)
                .take(size)
                .collect::<Vec<_>>();
            let mut songs: HashMap<_, _> = array(list, &["tracks"])
                .iter()
                .filter_map(|r| track(r, platform))
                .map(|t| (t.song_id.clone(), t))
                .collect();
            let missing = wanted
                .iter()
                .filter(|id| !songs.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                for t in self.netease_details(&missing).await? {
                    songs.insert(t.song_id.clone(), t);
                }
            }
            let items = wanted
                .iter()
                .filter_map(|id| songs.remove(id))
                .collect::<Vec<_>>();
            ensure!(
                items.len() == wanted.len(),
                "Incomplete playlist: missing track details"
            );
            return Ok(CollectionPage::new(items, total, page, size));
        }
        let d=match platform {
            Platform::Kw if chart => self.request("https://kbangserver.kuwo.cn/ksong.s", &[("from","pc".into()),("fmt","json".into()),("pn",(page-1).to_string()),("rn","100".into()),("type","bang".into()),("data","content".into()),("id",collection),("show_copyright_off","0".into()),("pcmp4","1".into()),("isbang","1".into())],None,false,&[]).await?,
            Platform::Kw => self.request("https://nplserver.kuwo.cn/pl.svc", &[("op","getlistinfo".into()),("pid",collection),("pn",(page-1).to_string()),("rn","100".into()),("encode","utf8".into()),("keyset","pl2012".into()),("identity","kuwo".into()),("pcmp4","1".into()),("vipver","MUSIC_9.0.5.0_W1".into()),("newver","1".into())],None,false,&[]).await?,
            Platform::Kg => self.request(&format!("http://mobilecdnbj.kugou.com/api/v3/{}/song",if chart {"rank"}else{"special"}), &[("version","9108".into()),("plat","0".into()),("page",page.to_string()),("pagesize","100".into()),(if chart {"rankid"}else{"specialid"},collection)],None,false,&[]).await?,
            Platform::Tx if chart => self.request("https://u.y.qq.com/cgi-bin/musicu.fcg", &[],Some(json!({"req":{"module":"musicToplist.ToplistInfoServer","method":"GetDetail","param":{"topid":collection.parse::<u64>()?,"offset":(page-1)*100,"num":100}}})),false,&[]).await?,
            Platform::Tx => self.request("https://c.y.qq.com/qzone/fcg-bin/fcg_ucc_getcdinfo_byids_cp.fcg", &[("type","1".into()),("json","1".into()),("utf8","1".into()),("onlysong","0".into()),("new_format","1".into()),("disstid",collection),("format","json".into())],None,false,&[]).await?,
            Platform::Mg if chart => self.request("https://app.c.nf.migu.cn/MIGUM2.0/v1.0/content/querycontentbyId.do", &[("columnId",collection),("needAll","0".into())],None,false,&[]).await?,
            Platform::Mg => self.request("https://app.c.nf.migu.cn/MIGUM3.0/resource/playlist/song/v2.0", &[("pageNo",page.to_string()),("pageSize","50".into()),("playlistId",collection)],None,false,&[]).await?,
            Platform::Wy => unreachable!(),
        };
        let (rows, total) = match platform {
            Platform::Kw => {
                ensure!(d["musiclist"].is_array(), "Kuwo list unavailable");
                (
                    array(&d, &["musiclist"]),
                    number(&d, &[if chart { "num" } else { "total" }]) as usize,
                )
            }
            Platform::Kg => {
                ensure!(d["status"] == 1, "Kugou list unavailable");
                (
                    array(&d["data"], &["info"]),
                    number(&d["data"], &["total"]) as usize,
                )
            }
            Platform::Tx if chart => {
                ensure!(d["req"]["code"] == 0, "QQ chart unavailable");
                (
                    array(&d["req"]["data"], &["songInfoList"]),
                    number(&d["req"]["data"]["data"], &["totalNum"]) as usize,
                )
            }
            Platform::Tx => {
                ensure!(
                    d["code"] == 0 && d["cdlist"][0].is_object(),
                    "QQ playlist unavailable"
                );
                let r = &d["cdlist"][0];
                let all = array(r, &["songlist"]);
                let total = number(r, &["total_song_num"]) as usize;
                ensure!(all.len() >= total, "QQ playlist response is truncated");
                (
                    all.into_iter()
                        .skip((page as usize - 1) * size)
                        .take(size)
                        .collect(),
                    total,
                )
            }
            Platform::Mg if chart => {
                ensure!(
                    d["code"] == "000000" && d["columnInfo"].is_object(),
                    "Migu chart unavailable"
                );
                let all = array(&d["columnInfo"], &["contents"])
                    .iter()
                    .map(|r| r["objectInfo"].clone())
                    .collect::<Vec<_>>();
                let total = all.len();
                (
                    all.into_iter()
                        .skip((page as usize - 1) * size)
                        .take(size)
                        .collect(),
                    total,
                )
            }
            Platform::Mg => {
                ensure!(
                    d["code"] == "000000" && d["data"]["songList"].is_array(),
                    "Migu playlist unavailable"
                );
                (
                    array(&d["data"], &["songList"]),
                    number(&d["data"], &["totalCount"]) as usize,
                )
            }
            Platform::Wy => unreachable!(),
        };
        let items = rows
            .iter()
            .filter_map(|r| track(r, platform))
            .collect::<Vec<_>>();
        ensure!(
            items.len() == rows.len(),
            "Incomplete list: malformed track metadata"
        );
        Ok(CollectionPage::new(items, total, page, size))
    }

    pub async fn all_tracks(&self, id: &str, chart: bool) -> Result<Vec<Track>> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut expected = None;
        for page in 1..=100 {
            let response = self.tracks(id, chart, page).await?;
            if let (Some(old), Some(total)) = (expected, response.total) {
                ensure!(old == total, "List changed while paging; retry");
            }
            expected = response.total;
            let previous = result.len();
            for track in response.items {
                if seen.insert(track.id.clone()) {
                    result.push(track);
                }
            }
            if !response.has_more {
                ensure!(
                    expected.is_none_or(|total| result.len() == total),
                    "Incomplete list: received {}/{} tracks",
                    result.len(),
                    expected.unwrap_or(0)
                );
                return Ok(result);
            }
            ensure!(
                result.len() > previous,
                "Incomplete list: paging made no progress"
            );
        }
        anyhow::bail!("Collection exceeds 100 pages; refusing a partial save")
    }
}

pub fn parse_id(id: &str) -> Result<(Platform, String)> {
    let (platform, id) = id.split_once(':').context("Expected platform:id")?;
    ensure!(
        !id.is_empty() && id.len() <= 256 && !id.chars().any(|c| c.is_control()),
        "Invalid platform ID"
    );
    Ok((platform.parse()?, id.to_string()))
}

fn flatten(value: &Value, key: &str) -> Vec<Value> {
    array(value, &[])
        .iter()
        .flat_map(|r| {
            if !r[key].is_null() {
                vec![r.clone()]
            } else {
                flatten(&r["contents"], key)
            }
        })
        .collect()
}
