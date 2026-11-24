// Runs inside music.youtube.com, which stays hidden and acts as the audio engine.
// It reports player and queue state, runs the page's own API calls with the signed-in
// session, and drives the page's real queue. Credentials never leave the page.
// Ads are never skipped: seek and track changes are no-ops while one is playing.
(() => {
  if (window.__drift || location.hostname !== "music.youtube.com") return;

  const ENDPOINTS = new Set([
    "browse", "search", "next", "music/get_search_suggestions", "music/get_queue",
    "like/like", "like/dislike", "like/removelike", "feedback", "browse/edit_playlist",
    "playlist/create", "playlist/delete", "playlist/get_add_to_playlist",
    "subscription/subscribe", "subscription/unsubscribe", "account/account_menu",
    "account/get_setting", "account/set_setting",
  ]);
  const DROP = new Set([
    "trackingParams", "clickTrackingParams", "loggingContext", "responseContext", "accessibility",
    "accessibilityData", "accessibilityPauseData", "accessibilityPlayData", "loggingDirectives",
  ]);

  const $ = (selector) => document.querySelector(selector);
  const player = () => document.getElementById("movie_player");
  const video = () => $("video");
  const queueApi = () => $("ytmusic-player-queue")?.queue;
  const store = () => queueApi()?.store?.store;
  const queue = () => store()?.getState()?.queue;
  const isAd = () => !!player()?.classList.contains("ad-showing");
  // YouTube Music's own Song / Video switch, shown when a song has a music video.
  const avToggle = () => $("ytmusic-av-toggle");
  const clipMode = () => avToggle()?.getAttribute("is-video-playback-mode-selected") === "true";
  const clipAvailable = () => {
    const toggle = avToggle();
    return !!toggle && !toggle.hasAttribute("toggle-disabled") &&
      (toggle.hasAttribute("selected-item-has-video") || toggle.hasAttribute("selected-queue-item-has-counterpart"));
  };
  const finite = (value) => (Number.isFinite(value) ? value : 0);
  const post = (message) => window.ipc?.postMessage(
    JSON.stringify(message, (key, value) => (DROP.has(key) ? undefined : value)),
  );

  const unwrap = (item) =>
    item?.playlistPanelVideoRenderer ??
    item?.playlistPanelVideoWrapperRenderer?.primaryRenderer?.playlistPanelVideoRenderer ??
    null;
  const runs = (value) => value?.simpleText ?? value?.runs?.map((run) => run.text).join("") ?? "";
  const lastThumb = (value) => value?.thumbnails?.at(-1)?.url ?? "";

  // ---- InnerTube -----------------------------------------------------------------------

  const cookie = (name) =>
    document.cookie.split("; ").find((c) => c.startsWith(`${name}=`))?.slice(name.length + 1);

  const authorization = async () => {
    const sid = cookie("__Secure-3PAPISID") || cookie("SAPISID");
    if (!sid) return null;
    const now = Math.floor(Date.now() / 1000);
    const data = new TextEncoder().encode(`${now} ${sid} ${location.origin}`);
    const digest = await crypto.subtle.digest("SHA-1", data);
    const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
    return `SAPISIDHASH ${now}_${hex}`;
  };

  const LANGUAGE_KEY = "drift-hl";
  const SAFETY_BIT = 0x8000000;

  const chosenLanguage = () => {
    try {
      return localStorage.getItem(LANGUAGE_KEY) || "en";
    } catch {
      return "en";
    }
  };

  const readPref = () => new URLSearchParams(cookie("PREF") ?? "");
  const writePref = (pref) => {
    document.cookie = `PREF=${pref.toString()}; domain=.youtube.com; path=/; max-age=63072000; secure; samesite=lax`;
  };
  const safetyMode = () => (parseInt(readPref().get("f2") ?? "0", 16) & SAFETY_BIT) !== 0;

  // Language, region and restricted mode live in the PREF cookie and need a page reload.
  const applyClientSettings = ({ settingDatas }) => {
    const pref = readPref();
    for (const data of settingDatas ?? []) {
      const item = data.clientSettingEnum?.item;
      if (item === "I18N_LANGUAGE") {
        pref.set("hl", data.stringValue ?? "");
        localStorage.setItem(LANGUAGE_KEY, data.stringValue ?? "");
      } else if (item === "I18N_REGION") {
        pref.set("gl", data.stringValue ?? "");
      } else if (item === "SAFETY_MODE") {
        const flags = parseInt(pref.get("f2") ?? "0", 16) || 0;
        const next = data.boolValue ? flags | SAFETY_BIT : flags & ~SAFETY_BIT;
        if (next) pref.set("f2", next.toString(16));
        else pref.delete("f2");
      }
    }
    writePref(pref);
    setTimeout(() => location.reload(), 300);
    return null;
  };

  const innertube = async (endpoint, body) => {
    if (!ENDPOINTS.has(endpoint)) throw new Error(`endpoint not allowed: ${endpoint}`);
    const cfg = window.ytcfg;
    const base = cfg.get("INNERTUBE_CONTEXT");
    // English matches the app's interface until the user picks a language in settings.
    const context = { ...base, client: { ...base.client, hl: chosenLanguage() } };
    const headers = {
      "Content-Type": "application/json",
      "X-Goog-AuthUser": String(cfg.get("SESSION_INDEX") ?? 0),
      "X-Origin": location.origin,
      "X-Youtube-Client-Name": String(cfg.get("INNERTUBE_CONTEXT_CLIENT_NAME")),
      "X-Youtube-Client-Version": String(cfg.get("INNERTUBE_CLIENT_VERSION")),
    };
    const auth = await authorization();
    if (auth) headers.Authorization = auth;
    const pageId = cfg.get("DELEGATED_SESSION_ID");
    if (pageId) headers["X-Goog-PageId"] = pageId;
    const response = await fetch(`/youtubei/v1/${endpoint}?prettyPrint=false`, {
      method: "POST",
      credentials: "include",
      headers,
      body: JSON.stringify({ context, ...body }),
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return response.json();
  };

  const reply = (id, promise) => {
    promise.then(
      (data) => post({ kind: "api", id, ok: true, data: data ?? null }),
      (error) => post({ kind: "api", id, ok: false, error: String(error?.message ?? error) }),
    );
  };

  // ---- Queue ---------------------------------------------------------------------------

  let continuation = null;
  let fetchingMore = false;

  // The page's reducer does not shift the selection when items move, so callers do.
  const setIndex = (index) => store().dispatch({ type: "SET_INDEX", payload: index });

  const addItems = (items, index) => {
    const q = queue();
    if (!q || !items.length) return;
    store().dispatch({
      type: "ADD_ITEMS",
      payload: { nextQueueItemId: q.nextQueueItemId, index, items, shuffleEnabled: false, shouldAssignIds: true },
    });
  };

  const panelOf = (json) =>
    json?.contents?.singleColumnMusicWatchNextResultsRenderer?.tabbedRenderer
      ?.watchNextTabbedResultsRenderer?.tabs?.[0]?.tabRenderer?.content?.musicQueueRenderer
      ?.content?.playlistPanelRenderer;

  const continuationOf = (panel) => {
    const next = panel?.continuations?.[0];
    return next?.nextRadioContinuationData?.continuation ?? next?.nextContinuationData?.continuation ?? null;
  };

  // The page starts a track when the selected index changes. That start carries the queue's
  // playlist, so the server remembers the whole queue instead of a single track.
  const startAt = (index) => {
    const q = queue();
    const target = unwrap(q?.items?.[index]);
    if (!target) return;
    queueApi().unblockPlaybackAutostart?.();
    if (q.selectedItemIndex !== index) {
      setIndex(index);
    } else if (q.items.length > 1) {
      setIndex(index === 0 ? 1 : 0);
      setIndex(index);
    } else {
      player().loadVideoById(target.videoId);
    }
  };

  const startCurrent = () => {
    const p = player();
    const q = queue();
    const current = unwrap(q?.items?.[q.selectedItemIndex]);
    if (!p || !current) return;
    if (p.getPlayerState() === -1 || p.getVideoData()?.video_id !== current.videoId) {
      startAt(q.selectedItemIndex);
    } else {
      p.playVideo();
    }
  };

  const select = (index) => {
    const q = queue();
    const item = q?.items?.[index];
    if (!item || isAd()) return;
    if (index === q.selectedItemIndex) startCurrent();
    else queueApi().selectQueueItem(item);
  };

  // Plays a watch endpoint by loading its queue from the API into the page's real queue.
  const play = async (endpoint) => {
    if (isAd()) throw new Error("ad is playing");
    const json = await innertube("next", {
      enablePersistentPlaylistPanel: true,
      isAudioOnly: true,
      tunerSettingValue: "AUTOMIX_SETTING_NORMAL",
      videoId: endpoint.videoId,
      // A single track continues as its radio, like in the official player.
      playlistId: endpoint.playlistId ?? (endpoint.videoId ? `RDAMVM${endpoint.videoId}` : undefined),
      params: endpoint.params ?? (endpoint.playlistId ? undefined : "wAEB"),
      index: endpoint.index,
      playlistSetVideoId: endpoint.playlistSetVideoId,
    });
    const panel = panelOf(json);
    const items = (panel?.contents ?? []).filter(unwrap);
    if (!items.length) throw new Error("empty queue");
    continuation = continuationOf(panel);
    const start = Math.max(0, items.findIndex((item) => unwrap(item).selected));
    const previous = queue().items.length;
    addItems(items, previous);
    // Ties later queue edits to the server-side queue this response created.
    store().dispatch({ type: "SET_QUEUE_CONTEXT_PARAMS", payload: json.queueContextParams ?? "" });
    store().dispatch({ type: "SET_IS_INFINITE", payload: !!panel?.isInfinite });
    if (previous === 0) {
      startAt(start);
    } else {
      setIndex(previous + start);
      store().dispatch({ type: "CLEAR", payload: items.map((_, offset) => previous + offset) });
      setIndex(start);
    }
    return null;
  };

  const loadMore = async () => {
    const q = queue();
    if (!q || !continuation || fetchingMore || q.items.length - q.selectedItemIndex > 6) return;
    fetchingMore = true;
    try {
      const json = await innertube("next", {
        continuation, enablePersistentPlaylistPanel: true, isAudioOnly: true,
      });
      const panel = json?.continuationContents?.playlistPanelContinuation;
      continuation = continuationOf(panel);
      addItems((panel?.contents ?? []).filter(unwrap), queue().items.length);
    } catch {
      continuation = null;
    } finally {
      fetchingMore = false;
    }
  };

  const addToQueue = async ({ queueTarget, queueInsertPosition, params }) => {
    const body = queueTarget?.videoId
      ? { videoIds: [queueTarget.videoId], playlistId: queueTarget.playlistId, params }
      : { playlistId: queueTarget?.playlistId, params };
    const json = await innertube("music/get_queue", body);
    const items = (json?.queueDatas ?? []).map((data) => data.content).filter(unwrap);
    const q = queue();
    const wasEmpty = !q?.items?.length;
    const index = queueInsertPosition === "INSERT_AFTER_CURRENT_VIDEO" && !wasEmpty ? q.selectedItemIndex + 1 : q?.items?.length ?? 0;
    addItems(items, index);
    if (wasEmpty && queue()?.items?.length) queueApi().selectQueueItem(queue().items[0]);
    return null;
  };

  const serviceCall = (endpoint) => {
    const e = endpoint;
    if (e.feedbackEndpoint) return ["feedback", { feedbackTokens: [e.feedbackEndpoint.feedbackToken] }];
    if (e.likeEndpoint) {
      const status = e.likeEndpoint.status;
      const path = status === "LIKE" ? "like/like" : status === "DISLIKE" ? "like/dislike" : "like/removelike";
      return [path, { target: e.likeEndpoint.target }];
    }
    if (e.playlistEditEndpoint) {
      const { playlistId, actions, params } = e.playlistEditEndpoint;
      return ["browse/edit_playlist", { playlistId, actions, params }];
    }
    if (e.subscribeEndpoint) return ["subscription/subscribe", { channelIds: e.subscribeEndpoint.channelIds, params: e.subscribeEndpoint.params }];
    if (e.unsubscribeEndpoint) return ["subscription/unsubscribe", { channelIds: e.unsubscribeEndpoint.channelIds, params: e.unsubscribeEndpoint.params }];
    if (e.deletePlaylistEndpoint) return ["playlist/delete", { playlistId: e.deletePlaylistEndpoint.playlistId }];
    if (e.setSettingEndpoint) {
      return ["account/set_setting", { newValue: e.setSettingEndpoint, settingItemId: e.setSettingEndpoint.settingItemId }];
    }
    return null;
  };

  const service = async (endpoint) => {
    if (endpoint.queueAddEndpoint) return addToQueue(endpoint.queueAddEndpoint);
    if (endpoint.setClientSettingEndpoint) return applyClientSettings(endpoint.setClientSettingEndpoint);
    const call = serviceCall(endpoint);
    if (!call) throw new Error("unsupported action");
    const result = await innertube(call[0], call[1]);
    if (endpoint.likeEndpoint?.target?.videoId) {
      const { status, target } = endpoint.likeEndpoint;
      store()?.dispatch({ type: "SET_LIKE_STATUS", payload: { videoId: target.videoId, likeStatus: status } });
      likeOverrides.set(target.videoId, status);
    }
    return result;
  };

  // ---- Player state --------------------------------------------------------------------

  const likeOverrides = new Map();

  const likeOf = (videoId) => {
    if (!videoId) return "INDIFFERENT";
    return likeOverrides.get(videoId) ?? store()?.getState()?.likeStatus?.videos?.[videoId] ?? "INDIFFERENT";
  };

  const snapshot = () => {
    const v = video();
    const p = player();
    const q = queue();
    const meta = navigator.mediaSession?.metadata;
    const selected = unwrap(q?.items?.[q?.selectedItemIndex]);
    const ad = isAd();
    const loaded = !!v?.currentSrc && (p?.getPlayerState?.() ?? -1) !== -1;
    const useMeta = loaded && !!meta?.title;
    const artwork = meta?.artwork ?? [];
    const videoId = ad ? selected?.videoId ?? "" : p?.getVideoData?.()?.video_id || selected?.videoId || "";
    return {
      kind: "state",
      signedIn: !!window.ytcfg?.get?.("LOGGED_IN"),
      hasTrack: !!(useMeta || selected),
      playing: !!v && !v.paused && !v.ended,
      position: loaded ? finite(v?.currentTime) : 0,
      duration: loaded ? finite(v?.duration) : 0,
      volume: typeof p?.getVolume === "function" ? finite(p.getVolume()) : 100,
      muted: typeof p?.isMuted === "function" ? !!p.isMuted() : !!v?.muted,
      title: useMeta ? meta.title : runs(selected?.title),
      artist: useMeta ? meta.artist : runs(selected?.shortBylineText),
      album: useMeta ? meta.album ?? "" : "",
      artwork: useMeta && artwork.length ? artwork.at(-1).src : lastThumb(selected?.thumbnail),
      isAd: ad,
      visibility: document.visibilityState,
      url: location.href,
      videoId,
      like: likeOf(videoId),
      repeat: q?.repeatMode ?? "NONE",
      shuffle: !!q?.shuffleEnabled,
      safetyMode: safetyMode(),
      language: chosenLanguage(),
      region: window.ytcfg?.get?.("INNERTUBE_CONTEXT")?.client?.gl ?? "",
      clipAvailable: clipMode() || clipAvailable(),
      clipMode: clipMode(),
    };
  };

  // The newest picture of the music video, as a small JPEG, for Drift to turn into stars.
  // Only while the video plays and no ad runs. Pictures are read from the page's own video and never stored.
  const FRAME_WIDTH = 384;
  const FRAME_HEIGHT = 216;
  const FRAME_QUALITY = 0.85;
  // Each new picture is sent as it appears, but never more often than this many milliseconds apart,
  // counted from the start of one picture to the start of the next: a 60 fps video still sends 30 a second.
  const FRAME_GAP = 28;
  let frameCanvas = null;
  let lastFrame = -1;
  let lastFrameAt = 0;
  const frame = () => {
    try {
      return readFrame();
    } catch {
      return "";
    }
  };
  const sendFrame = () => {
    const now = performance.now();
    if (now - lastFrameAt < FRAME_GAP) return;
    const jpeg = frame();
    if (!jpeg) return;
    lastFrameAt = now;
    post({ kind: "frame", jpeg });
  };
  const readFrame = () => {
    const v = video();
    if (!v || isAd() || !clipMode() || !v.videoWidth || v.readyState < 2) return "";
    // The video's own clock tells a new picture. WebKit's frame counter only moves a few times a second.
    const stamp = v.currentTime;
    if (stamp === lastFrame) return "";
    lastFrame = stamp;
    frameCanvas ??= Object.assign(document.createElement("canvas"), { width: FRAME_WIDTH, height: FRAME_HEIGHT });
    const context = frameCanvas.getContext("2d");
    const { videoWidth: width, videoHeight: height } = v;
    const wanted = FRAME_WIDTH / FRAME_HEIGHT;
    const cropWidth = Math.min(width, height * wanted);
    const cropHeight = Math.min(height, width / wanted);
    context.drawImage(v, (width - cropWidth) / 2, (height - cropHeight) / 2, cropWidth, cropHeight, 0, 0, FRAME_WIDTH, FRAME_HEIGHT);
    const url = frameCanvas.toDataURL("image/jpeg", FRAME_QUALITY);
    return url.slice(url.indexOf(",") + 1);
  };

  // Drift renews the stream every few seconds. If it stops asking (it quit, or the clip was
  // switched off), the page stops sending by itself.
  const STREAM_LEASE = 6000;
  let streamUntil = 0;
  let streamTimer = null;
  // The video says when it shows a new picture, and each one is sent right then. Every new chain of
  // callbacks retires the one before it, so a swapped or restarted video never gets two.
  const callbacks = "requestVideoFrameCallback" in HTMLVideoElement.prototype;
  let followed = null;
  let chain = 0;
  const follow = () => {
    const v = video();
    if (!v || followed === v) return;
    followed = v;
    const mine = ++chain;
    const next = () => {
      if (mine !== chain) return;
      sendFrame();
      v.requestVideoFrameCallback(next);
    };
    v.requestVideoFrameCallback(next);
  };
  // A slow timer picks up a new video element when the page swaps it, and ends the stream once Drift
  // stops renewing it. Where the video cannot say when it has a new picture, the timer samples it instead.
  const tick = () => {
    if (performance.now() > streamUntil) {
      clearInterval(streamTimer);
      streamTimer = null;
      followed = null;
      chain += 1;
      return;
    }
    if (callbacks) follow();
    else sendFrame();
  };

  let lastSent = 0;
  const report = (force = false) => {
    const now = performance.now();
    if (!force && now - lastSent < 250) return;
    lastSent = now;
    post(snapshot());
    loadMore();
  };

  let queueSignature = "";
  const reportQueue = () => {
    const q = queue();
    if (!q) return;
    const items = q.items.map(unwrap).filter(Boolean);
    const signature = `${q.selectedItemIndex}|${items.map((i) => `${i.videoId}:${i.playlistSetVideoId ?? ""}`).join(",")}`;
    if (signature === queueSignature) return;
    queueSignature = signature;
    post({
      kind: "queue",
      index: q.selectedItemIndex ?? 0,
      items: items.map((item) => ({
        videoId: item.videoId,
        title: runs(item.title),
        artist: runs(item.shortBylineText),
        duration: runs(item.lengthText),
        thumbnail: item.thumbnail?.thumbnails?.[0]?.url ?? "",
      })),
    });
  };

  const commands = {
    toggle() {
      const p = player();
      if (!p) return;
      const state = p.getPlayerState();
      if (state === 1 || state === 3) p.pauseVideo();
      else if (state === -1 || state === 5) startCurrent();
      else p.playVideo();
    },
    next() {
      const q = queue();
      if (!q || isAd()) return;
      if (q.selectedItemIndex + 1 < q.items.length) select(q.selectedItemIndex + 1);
      else if (q.repeatMode === "ALL" && q.items.length) select(0);
      else player()?.nextVideo?.();
    },
    previous() {
      const q = queue();
      if (!q || isAd()) return;
      if (finite(video()?.currentTime) > 3 || q.selectedItemIndex === 0) player()?.seekTo?.(0, true);
      else select(q.selectedItemIndex - 1);
    },
    seek(seconds) {
      if (!isAd()) player()?.seekTo?.(seconds, true);
    },
    volume(level) {
      const p = player();
      if (typeof p?.setVolume !== "function") return;
      p.setVolume(level);
      if (level > 0 && p.isMuted?.()) p.unMute?.();
    },
    mute() {
      const p = player();
      if (typeof p?.isMuted === "function") (p.isMuted() ? p.unMute() : p.mute());
    },
    select,
    remove(index) {
      const q = queue();
      if (!q || index === q.selectedItemIndex || !q.items[index]) return;
      const selected = q.selectedItemIndex;
      // The page's own method also removes the track from the server-side queue.
      queueApi().removeItem(q.items[index]);
      if (index < selected) setIndex(selected - 1);
    },
    move({ from, to }) {
      const q = queue();
      if (!q?.items[from] || !q.items[to]) return;
      const selected = q.selectedItemIndex;
      const moved = unwrap(q.items[from]);
      store().dispatch({ type: "MOVE_ITEM", payload: { fromIndex: from, toIndex: to } });
      let next = selected;
      if (from === selected) next = to;
      else if (from < selected && to >= selected) next = selected - 1;
      else if (from > selected && to <= selected) next = selected + 1;
      if (next !== selected) setIndex(next);
      const predecessor = to > 0 ? unwrap(queue().items[to - 1]) : null;
      const playlistId = moved?.queueNavigationEndpoint?.queueAddEndpoint?.queueTarget?.backingQueuePlaylistId;
      if (playlistId && moved.playlistEditParams && moved.playlistSetVideoId) {
        innertube("browse/edit_playlist", {
          playlistId,
          params: moved.playlistEditParams,
          actions: [{
            action: "ACTION_MOVE_VIDEO_AFTER",
            setVideoId: moved.playlistSetVideoId,
            movedSetVideoIdPredecessor: predecessor?.playlistSetVideoId,
          }],
        }).catch(() => {});
      }
    },
    shuffle() {
      const q = queue();
      if (!q) return;
      store().dispatch({ type: "SET_SHUFFLE_ENABLED", payload: !q.shuffleEnabled });
      if (!q.shuffleEnabled) queueApi().shuffle?.();
    },
    repeat(mode) {
      store()?.dispatch({ type: "SET_REPEAT", payload: mode });
      player()?.setLoopVideo?.(mode === "ONE");
    },
    // Starts or stops sending the music video's frames, about 20 a second. Asking again keeps it going.
    stream(on) {
      streamUntil = on ? performance.now() + STREAM_LEASE : 0;
      if (on && !streamTimer) {
        streamTimer = setInterval(tick, callbacks ? 250 : 33);
        tick();
      }
    },
    // Flips the page's own Song / Video switch. The page keeps the choice for the songs after this one.
    clip(on) {
      const toggle = avToggle();
      if (!toggle || isAd() || !!on === clipMode()) return;
      if (on && !clipAvailable()) return;
      toggle.querySelector(on ? ".video-button" : ".song-button")?.click();
    },
  };

  // ---- The music video's own subtitles ------------------------------------------------------

  // Subtitles timed to the clip itself. The track YouTube marks as the clip's own comes first,
  // then a written one in the sung language, and the speech-recognized one last. Only YouTube is asked.
  const captions = async (wanted) => {
    let response = null;
    for (let tries = 0; tries < 15; tries++) {
      response = player()?.getPlayerResponse?.();
      if (response?.videoDetails?.videoId === wanted) break;
      await new Promise((done) => setTimeout(done, 200));
    }
    if (response?.videoDetails?.videoId !== wanted) return null;
    const list = response.captions?.playerCaptionsTracklistRenderer;
    const tracks = list?.captionTracks ?? [];
    const recognized = tracks.find((track) => track.kind === "asr");
    const written = tracks.filter((track) => track.kind !== "asr");
    const own = tracks[list?.audioTracks?.[list.defaultAudioTrackIndex ?? 0]?.defaultCaptionTrackIndex ?? -1];
    const track =
      (own && own.kind !== "asr" ? own : null) ??
      written.find((track) => recognized && track.languageCode === recognized.languageCode) ??
      (written.length === 1 ? written[0] : null) ??
      recognized;
    if (!track?.baseUrl) return { video: wanted, kind: "none" };
    const url = new URL(track.baseUrl, location.href);
    if (!["www.youtube.com", "music.youtube.com"].includes(url.hostname)) return { video: wanted, kind: "none" };
    url.searchParams.set("fmt", "json3");
    const answer = await fetch(url, { credentials: "include" });
    if (!answer.ok) throw new Error(`HTTP ${answer.status}`);
    return { video: wanted, kind: track.kind === "asr" ? "asr" : "written", body: await answer.text() };
  };

  window.__drift = {
    exec(name, arg) {
      commands[name]?.(arg);
      setTimeout(() => { report(true); reportQueue(); }, 80);
    },
    api: (id, endpoint, body) => reply(id, innertube(endpoint, body)),
    service: (id, endpoint) => reply(id, service(endpoint)),
    play: (id, endpoint) => reply(id, play(endpoint)),
    captions: (id, video) => reply(id, captions(video)),
    frame,
  };

  for (const event of ["play", "pause", "ended", "durationchange", "volumechange", "seeked", "loadedmetadata"]) {
    document.addEventListener(event, () => report(true), true);
  }
  document.addEventListener("timeupdate", () => report(), true);
  document.addEventListener("visibilitychange", () => report(true));

  let subscribed = null;
  setInterval(() => {
    const current = store();
    if (current && current !== subscribed) {
      subscribed = current;
      let pending = false;
      current.subscribe(() => {
        if (pending) return;
        pending = true;
        setTimeout(() => { pending = false; reportQueue(); report(true); }, 120);
      });
      reportQueue();
    }
    report(true);
  }, 1000);
})();
