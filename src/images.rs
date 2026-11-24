//! Downloads and caches artwork on background threads.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use futures::{StreamExt, channel::mpsc::unbounded};
use gpui::{App, AppContext, Context, Entity, Image, ImageFormat, Task};

const WORKERS: usize = 6;
const MAX_ENTRIES: usize = 900;
const MAX_BYTES: u64 = 6 * 1024 * 1024;
const BATCH: Duration = Duration::from_millis(70);

enum Slot {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

pub struct Images {
    slots: HashMap<String, Slot>,
    order: VecDeque<String>,
    requests: mpsc::Sender<String>,
    _results: Task<()>,
}

impl Images {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx: &mut Context<Self>| {
            let (requests, incoming) = mpsc::channel::<String>();
            let incoming = Arc::new(Mutex::new(incoming));
            let (done_tx, mut done_rx) = unbounded::<(String, Option<Image>)>();
            for _ in 0..WORKERS {
                let incoming = incoming.clone();
                let done_tx = done_tx.clone();
                thread::spawn(move || {
                    loop {
                        let next = incoming.lock().ok().and_then(|rx| rx.recv().ok());
                        let Some(url) = next else { return };
                        let image = fetch(&url);
                        if done_tx.unbounded_send((url, image)).is_err() {
                            return;
                        }
                    }
                });
            }
            let results = cx.spawn(async move |this, cx| {
                while let Some(first) = done_rx.next().await {
                    let mut batch = vec![first];
                    cx.background_executor().timer(BATCH).await;
                    while let Ok(more) = done_rx.try_recv() {
                        batch.push(more);
                    }
                    let updated = this.update(cx, |images, cx| {
                        for (url, image) in batch {
                            let slot = image.map_or(Slot::Failed, |image| Slot::Ready(Arc::new(image)));
                            images.slots.insert(url, slot);
                        }
                        cx.notify();
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            });
            Self { slots: HashMap::new(), order: VecDeque::new(), requests, _results: results }
        })
    }

    pub fn get(&mut self, url: &str) -> Option<Arc<Image>> {
        if url.is_empty() {
            return None;
        }
        match self.slots.get(url) {
            Some(Slot::Ready(image)) => return Some(image.clone()),
            Some(_) => return None,
            None => {}
        }
        while self.order.len() >= MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.slots.remove(&oldest);
            }
        }
        self.slots.insert(url.to_string(), Slot::Loading);
        self.order.push_back(url.to_string());
        let _ = self.requests.send(url.to_string());
        None
    }
}

/// Asks Google's image CDN for a size close to what is displayed.
pub fn sized(url: &str, size: u32) -> String {
    let is_cdn = url.contains("googleusercontent.com") || url.contains("ggpht.com");
    if !is_cdn {
        return url.to_string();
    }
    match url.rfind('=') {
        Some(split) if url[split..].contains('w') || url[split..].contains('s') => {
            // "p" makes the CDN crop to a true square, so rounded corners stay correct.
            format!("{}=w{size}-h{size}-p-l90-rj", &url[..split])
        }
        _ => url.to_string(),
    }
}

fn is_artwork_host(host: &str) -> bool {
    ["googleusercontent.com", "ytimg.com", "ggpht.com", "gstatic.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn fetch(url: &str) -> Option<Image> {
    let uri = url.parse::<wry::http::Uri>().ok()?;
    // Personal mixes keep their generated covers on the music host itself.
    let is_mix_art = uri.host() == Some("music.youtube.com") && uri.path().starts_with("/image/");
    if uri.scheme_str() != Some("https") || !(is_mix_art || is_artwork_host(uri.host()?)) {
        return None;
    }
    let mut response = ureq::get(url).call().ok()?;
    let bytes = response.body_mut().with_config().limit(MAX_BYTES).read_to_vec().ok()?;
    let format = match bytes.as_slice() {
        [0xff, 0xd8, ..] => ImageFormat::Jpeg,
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => ImageFormat::Webp,
        [b'G', b'I', b'F', ..] => ImageFormat::Gif,
        _ => return None,
    };
    Some(Image::from_bytes(format, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizes_cdn_urls_only() {
        assert_eq!(
            sized("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj", 240),
            "https://lh3.googleusercontent.com/abc=w240-h240-p-l90-rj"
        );
        let plain = "https://i.ytimg.com/vi/abc/hqdefault.jpg";
        assert_eq!(sized(plain, 240), plain);
    }

    #[test]
    fn rejects_other_hosts() {
        assert!(is_artwork_host("i.ytimg.com"));
        assert!(!is_artwork_host("ytimg.com.evil.example"));
    }
}
