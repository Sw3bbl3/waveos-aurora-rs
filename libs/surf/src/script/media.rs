use super::*;
use crate::media::{MediaSource, ObjectUrl, ReadyState};

pub(super) fn install(rt: &mut Realm, html: ObjRef) -> (ObjRef, ObjRef) {
    let media = rt.new_object_with(Some(html));
    interface(rt, "HTMLMediaElement", media, None);
    methods(
        rt,
        media,
        &[("_mediaState", 0, state), ("_mediaCommand", 1, command), ("_mediaBuffered", 0, element_buffered)],
    );
    let video = rt.new_object_with(Some(media));
    interface(rt, "HTMLVideoElement", video, None);
    let audio = rt.new_object_with(Some(media));
    interface(rt, "HTMLAudioElement", audio, None);
    // The prelude builds MediaSource, SourceBuffer and object URLs on these.
    let mse = rt.new_object();
    methods(
        rt,
        mse,
        &[
            ("create", 0, mse_create),
            ("addSourceBuffer", 2, mse_add),
            ("removeSourceBuffer", 2, mse_remove_buffer),
            ("append", 3, mse_append),
            ("remove", 4, mse_remove),
            ("abort", 2, mse_abort),
            ("buffered", 2, mse_buffered),
            ("set", 4, mse_set),
            ("get", 1, mse_get),
            ("setDuration", 2, mse_set_duration),
            ("setReady", 2, mse_set_ready),
            ("attach", 2, mse_attach),
            ("detach", 1, mse_detach),
            ("isTypeSupported", 1, mse_supported),
            ("objectURL", 3, object_url),
            ("revokeObjectURL", 1, revoke_object_url),
            ("blobURL", 1, blob_url_bytes),
        ],
    );
    let g = rt.global;
    rt.define(g, "__nebula_mse", Value::Object(mse), 0);
    (video, audio)
}

fn media_node(rt: &mut Realm, c: &Call) -> Result<NodeId, Value> {
    let n = this_node(rt, c)?;
    let valid = with_dom(rt, |_, dom| matches!(dom.doc.tag(n), "video" | "audio"));
    if !valid {
        return Err(rt.type_error("Illegal HTMLMediaElement invocation"));
    }
    Ok(n)
}

fn state(rt: &mut Realm, c: &Call) -> JsResult {
    let n = media_node(rt, c)?;
    with_dom(rt, |rt, dom| {
        let default_muted = dom.doc.element(n).is_some_and(|e| e.attr("muted").is_some());
        let source_duration = dom
            .doc
            .media
            .get(&n)
            .and_then(|s| s.source)
            .and_then(|id| dom.doc.media_sources.get(&id))
            .map(|m| m.duration);
        let s = dom.doc.media.entry(n).or_default();
        let duration = match source_duration {
            Some(d) if s.ready_state >= 1 => d,
            Some(_) => f64::NAN,
            None => s.duration,
        };
        let o = rt.new_object();
        for (key, value) in [
            ("paused", Value::Bool(s.paused)),
            ("ended", Value::Bool(s.ended)),
            ("currentTime", Value::Number(s.seek.unwrap_or(s.current_time))),
            ("duration", Value::Number(duration)),
            ("videoWidth", Value::Number(s.width as f64)),
            ("videoHeight", Value::Number(s.height as f64)),
            ("readyState", Value::Number(s.ready_state as f64)),
            ("currentSrc", Value::str(&s.src)),
            ("seeking", Value::Bool(s.seeking || s.seek.is_some())),
            ("muted", Value::Bool(s.muted.unwrap_or(default_muted))),
            ("volume", Value::Number(s.volume)),
            ("playbackRate", Value::Number(s.playback_rate)),
            ("error", s.error.as_deref().map(Value::str).unwrap_or(Value::Null)),
        ] {
            rt.define(o, key, value, DEFAULT);
        }
        Ok(Value::Object(o))
    })
}

fn command(rt: &mut Realm, c: &Call) -> JsResult {
    let n = media_node(rt, c)?;
    let op = arg_str(rt, c, 0)?;
    let time = if matches!(op.as_str(), "seek" | "volume" | "muted" | "rate") { rt.to_number(&c.arg(1))? } else { 0.0 };
    if !time.is_finite() {
        return Err(rt.type_error("Invalid media time"));
    }
    with_dom(rt, |_, dom| {
        let s = dom.doc.media.entry(n).or_default();
        match op.as_str() {
            "play" => {
                s.paused = false;
                if s.ended {
                    s.seek = Some(0.0);
                    s.ended = false;
                }
            }
            "pause" => s.paused = true,
            "volume" => s.volume = time.clamp(0.0, 1.0),
            "muted" => s.muted = Some(time != 0.0),
            "rate" => s.playback_rate = time.clamp(0.0625, 16.0),
            "seek" => {
                s.seek = Some(time.max(0.0));
                s.ended = false;
            }
            "load" => {
                s.revision = s.revision.wrapping_add(1);
                s.error = None;
                s.ready_state = 0;
                s.current_time = 0.0;
                s.seek = None;
                s.seeking = false;
                s.ended = false;
                s.duration = f64::NAN;
                s.width = 0;
                s.height = 0;
            }
            _ => {}
        }
    });
    Ok(Value::Undefined)
}

fn ranges(rt: &mut Realm, r: Vec<(f64, f64)>) -> Value {
    let flat: Vec<Value> = r.into_iter().flat_map(|(s, e)| [Value::Number(s), Value::Number(e)]).collect();
    rt.array_from(flat)
}

fn element_buffered(rt: &mut Realm, c: &Call) -> JsResult {
    let n = media_node(rt, c)?;
    let r = with_dom(rt, |_, dom| {
        let s = dom.doc.media.get(&n);
        match s.and_then(|s| s.source).and_then(|id| dom.doc.media_sources.get(&id)) {
            Some(ms) => ms.element_buffered(),
            None => match s {
                Some(s) if s.ready_state >= 2 && s.duration.is_finite() => vec![(0.0, s.duration)],
                _ => Vec::new(),
            },
        }
    });
    Ok(ranges(rt, r))
}

fn ms_id(rt: &mut Realm, c: &Call, i: usize) -> Result<u32, Value> {
    Ok(rt.to_number(&c.arg(i))? as u32)
}

fn with_source<R>(rt: &mut Realm, id: u32, f: impl FnOnce(&mut MediaSource) -> R) -> Result<R, Value> {
    let r = with_dom(rt, |_, dom| dom.doc.media_sources.get_mut(&id).map(f));
    r.ok_or_else(|| rt.type_error("MediaSource is gone"))
}

fn mse_create(rt: &mut Realm, _c: &Call) -> JsResult {
    let id = with_dom(rt, |_, dom| {
        dom.next_id += 1;
        let id = dom.next_id;
        dom.doc.media_sources.insert(id, MediaSource::new(id));
        id
    });
    Ok(Value::Number(id as f64))
}

fn mse_add(rt: &mut Realm, c: &Call) -> JsResult {
    let id = ms_id(rt, c, 0)?;
    let mime = arg_str(rt, c, 1)?;
    let sb = with_dom(rt, |_, dom| {
        dom.next_id += 1;
        dom.next_id
    });
    match with_source(rt, id, |ms| ms.add_buffer(sb, &mime))? {
        Ok(()) => Ok(Value::Number(sb as f64)),
        Err(e) => Ok(Value::str(e)),
    }
}

fn mse_remove_buffer(rt: &mut Realm, c: &Call) -> JsResult {
    let (id, sb) = (ms_id(rt, c, 0)?, ms_id(rt, c, 1)?);
    with_source(rt, id, |ms| {
        ms.buffers.retain(|b| b.id != sb);
        ms.revision += 1;
    })?;
    Ok(Value::Undefined)
}

fn mse_append(rt: &mut Realm, c: &Call) -> JsResult {
    let (id, sb) = (ms_id(rt, c, 0)?, ms_id(rt, c, 1)?);
    let bytes = match c.arg(2) {
        Value::Object(o) => rt.buffer_source_bytes(o),
        _ => None,
    };
    let Some(bytes) = bytes else { return Err(rt.type_error("appendBuffer: not a BufferSource")) };
    match with_source(rt, id, |ms| ms.append(sb, &bytes))? {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Ok(Value::str(e)),
    }
}

fn mse_remove(rt: &mut Realm, c: &Call) -> JsResult {
    let (id, sb) = (ms_id(rt, c, 0)?, ms_id(rt, c, 1)?);
    let (start, end) = (rt.to_number(&c.arg(2))?, rt.to_number(&c.arg(3))?);
    with_source(rt, id, |ms| ms.remove(sb, start, end))?;
    Ok(Value::Undefined)
}

fn mse_abort(rt: &mut Realm, c: &Call) -> JsResult {
    let (id, sb) = (ms_id(rt, c, 0)?, ms_id(rt, c, 1)?);
    with_source(rt, id, |ms| ms.abort(sb))?;
    Ok(Value::Undefined)
}

fn mse_buffered(rt: &mut Realm, c: &Call) -> JsResult {
    let (id, sb) = (ms_id(rt, c, 0)?, ms_id(rt, c, 1)?);
    let r = with_source(rt, id, |ms| ms.buffered(sb))?;
    Ok(ranges(rt, r))
}

fn mse_set(rt: &mut Realm, c: &Call) -> JsResult {
    let (id, sb) = (ms_id(rt, c, 0)?, ms_id(rt, c, 1)?);
    let key = arg_str(rt, c, 2)?;
    let value = c.arg(3);
    let num = rt.to_number(&value)?;
    let mode = if key == "mode" { arg_str(rt, c, 3)? } else { String::new() };
    with_source(rt, id, |ms| {
        if let Some(b) = ms.buffer(sb) {
            match key.as_str() {
                "timestampOffset" => b.timestamp_offset = num,
                "appendWindowStart" => b.window_start = num,
                "appendWindowEnd" => b.window_end = num,
                "mode" => b.sequence = mode == "sequence",
                _ => {}
            }
        }
    })?;
    Ok(Value::Undefined)
}

fn mse_get(rt: &mut Realm, c: &Call) -> JsResult {
    let id = ms_id(rt, c, 0)?;
    let (ready, duration) = with_source(rt, id, |ms| (ms.ready, ms.duration))?;
    let o = rt.new_object();
    let ready = match ready {
        ReadyState::Closed => "closed",
        ReadyState::Open => "open",
        ReadyState::Ended => "ended",
    };
    rt.define(o, "readyState", Value::str(ready), DEFAULT);
    rt.define(o, "duration", Value::Number(duration), DEFAULT);
    Ok(Value::Object(o))
}

fn mse_set_duration(rt: &mut Realm, c: &Call) -> JsResult {
    let id = ms_id(rt, c, 0)?;
    let d = rt.to_number(&c.arg(1))?;
    with_source(rt, id, |ms| {
        ms.duration = d;
        ms.revision += 1;
    })?;
    Ok(Value::Undefined)
}

fn mse_set_ready(rt: &mut Realm, c: &Call) -> JsResult {
    let id = ms_id(rt, c, 0)?;
    let s = arg_str(rt, c, 1)?;
    with_source(rt, id, |ms| {
        ms.ready = match s.as_str() {
            "open" => ReadyState::Open,
            "ended" => ReadyState::Ended,
            _ => ReadyState::Closed,
        };
        ms.revision += 1;
    })?;
    Ok(Value::Undefined)
}

fn mse_attach(rt: &mut Realm, c: &Call) -> JsResult {
    let id = ms_id(rt, c, 0)?;
    let n = arg_node(rt, c, 1)?;
    with_dom(rt, |_, dom| {
        if let Some(ms) = dom.doc.media_sources.get_mut(&id) {
            ms.element = Some(n);
            ms.ready = ReadyState::Open;
        }
        let s = dom.doc.media.entry(n).or_default();
        s.source = Some(id);
    });
    Ok(Value::Undefined)
}

fn mse_detach(rt: &mut Realm, c: &Call) -> JsResult {
    let id = ms_id(rt, c, 0)?;
    with_dom(rt, |_, dom| {
        if let Some(ms) = dom.doc.media_sources.get_mut(&id) {
            if let Some(n) = ms.element.take() {
                if let Some(s) = dom.doc.media.get_mut(&n) {
                    if s.source == Some(id) {
                        s.source = None;
                    }
                }
            }
            ms.ready = ReadyState::Closed;
            ms.buffers.clear();
            ms.duration = f64::NAN;
        }
    });
    Ok(Value::Undefined)
}

fn mse_supported(rt: &mut Realm, c: &Call) -> JsResult {
    let t = arg_str(rt, c, 0)?;
    Ok(Value::Bool(crate::media::supported_type(&t)))
}

/// `objectURL(kind, payload, type)`: kind "blob" with bytes, or
/// "mediasource" with an id. Returns the new `blob:` URL.
fn object_url(rt: &mut Realm, c: &Call) -> JsResult {
    let kind = arg_str(rt, c, 0)?;
    let entry = if kind == "mediasource" {
        ObjectUrl::MediaSource(ms_id(rt, c, 1)?)
    } else {
        let bytes = match c.arg(1) {
            Value::Object(o) => rt.buffer_source_bytes(o).unwrap_or_default(),
            _ => Vec::new(),
        };
        let ty = if c.arg(2).is_nullish() { String::new() } else { arg_str(rt, c, 2)? };
        ObjectUrl::Blob { bytes: Arc::from(bytes), kind: ty }
    };
    let mut id = [0u8; 16];
    rt.random_bytes(&mut id);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    let uuid = format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]);
    let url = with_dom(rt, |_, dom| {
        let origin = Url::parse(&dom.url)
            .filter(|u| u.scheme == "http" || u.scheme == "https")
            .map(|u| match u.port {
                Some(p) => format!("{}://{}:{p}", u.scheme, u.host),
                None => format!("{}://{}", u.scheme, u.host),
            })
            .unwrap_or_else(|| String::from("null"));
        let url = format!("blob:{origin}/{uuid}");
        dom.doc.object_urls.insert(url.clone(), entry);
        url
    });
    Ok(Value::str(&url))
}

fn revoke_object_url(rt: &mut Realm, c: &Call) -> JsResult {
    let url = arg_str(rt, c, 0)?;
    with_dom(rt, |_, dom| dom.doc.object_urls.remove(&url));
    Ok(Value::Undefined)
}

/// The bytes and type behind a blob URL: `[ArrayBuffer, type]`, or null.
fn blob_url_bytes(rt: &mut Realm, c: &Call) -> JsResult {
    let url = arg_str(rt, c, 0)?;
    let found = with_dom(rt, |_, dom| match dom.doc.object_urls.get(&url) {
        Some(ObjectUrl::Blob { bytes, kind }) => Some((bytes.to_vec(), kind.clone())),
        _ => None,
    });
    let Some((bytes, kind)) = found else { return Ok(Value::Null) };
    let buf = rt.new_array_buffer(bytes);
    Ok(rt.array_from(vec![Value::Object(buf), Value::str(&kind)]))
}
