import argparse
import base64
import fcntl
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
from pathlib import Path
import sys
import tempfile
import time
import zlib
from urllib.parse import urlencode
from urllib.request import urlopen

from ytmusicapi import OAuthCredentials, YTMusic
from ytmusicapi.exceptions import YTMusicServerError


CONFIG = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "lintel"
CLIENT = CONFIG / "youtube-client.json"
OAUTH = CONFIG / "youtube-oauth.json"
SESSION = CONFIG / "youtube-browser.json"
CACHE = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "lintel" / "playlists"
LIBRARY = CACHE.parent / "library.json"
ORIGIN = "https://music.youtube.com"
SIGN_IN_URL = (
    "https://accounts.google.com/ServiceLogin?ltmpl=music&service=youtube&passive=true"
    "&continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue"
    "%26next%3Dhttps%253A%252F%252Fmusic.youtube.com%252F"
)

class ExpiredSession(Exception):
    pass


class SignInError(Exception):
    pass


class PlaylistEditError(Exception):
    pass


def connect_library():
    if SESSION.is_file():
        return YTMusic(str(SESSION))
    data = json.loads(CLIENT.read_text())
    client = OAuthCredentials(data["client_id"], data["client_secret"])
    return YTMusic(str(OAUTH), oauth_credentials=client)


class Browser:
    def __init__(self, transport):
        self.transport = transport
        self.pending = b""
        self.sequence = 0

    def call(self, method, params=None, session=None):
        self.sequence += 1
        request = {"id": self.sequence, "method": method, "params": params or {}}
        if session is not None:
            request["sessionId"] = session
        self.transport.sendall(json.dumps(request).encode() + b"\0")
        while True:
            while b"\0" not in self.pending:
                chunk = self.transport.recv(65536)
                if not chunk:
                    raise EOFError
                self.pending += chunk
            raw, self.pending = self.pending.split(b"\0", 1)
            response = json.loads(raw)
            if response.get("id") == self.sequence:
                return response.get("result", {})


def browser_session(browser, on_closed):
    with tempfile.TemporaryDirectory(prefix="lintel-login-", dir=os.environ.get("XDG_RUNTIME_DIR")) as profile:
        environment = {
            **os.environ, "XDG_CONFIG_HOME": profile, "XDG_CACHE_HOME": profile,
            "XDG_DATA_HOME": profile, "XDG_STATE_HOME": profile,
        }
        command = [
            browser, f"--user-data-dir={profile}", "--no-first-run",
            "--no-default-browser-check", "--disable-sync", "--disable-background-mode",
        ]
        # Account sign-in runs in a normal browser window. Connect to the dedicated
        # profile only after that window closes, to read the YouTube Music session.
        try:
            result = subprocess.run(
                [*command, "--new-window", SIGN_IN_URL], env=environment, timeout=600,
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        except subprocess.TimeoutExpired as error:
            raise SignInError("YouTube Music sign-in timed out.") from error
        if result.returncode:
            raise SignInError("The sign-in browser closed unexpectedly.")
        on_closed()
        return read_browser_session(command, environment)


def read_browser_session(command, environment):
    local, remote = socket.socketpair()
    with local, remote:
        # Chromium's debugging transport stays on inherited file descriptors.
        fd = remote.fileno()
        redirections = f"3<&{fd} 4>&{fd}"
        if fd > 4:
            redirections += f" {fd}<&-"
        process = subprocess.Popen(
            [
                "sh", "-c", f'exec {redirections}; exec "$@"', "lintel-login",
                # Use Chromium's normal browser mode with an offscreen platform.
                *command, "--ozone-platform=headless", "--disable-gpu",
                "--restore-last-session", "--remote-debugging-pipe", ORIGIN + "/",
            ],
            env=environment, pass_fds=(fd,), stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        remote.close()
        with process:
            local.settimeout(8)
            connection = Browser(local)
            try:
                return wait_for_session(connection, process)
            except (EOFError, TimeoutError) as error:
                raise SignInError("Could not load the YouTube Music session. Try signing in again.") from error
            finally:
                if process.poll() is None:
                    try:
                        local.settimeout(0.5)
                        connection.call("Browser.close")
                    except (OSError, EOFError):
                        pass
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()


def wait_for_session(browser, process):
    target_id = None
    session = None
    deadline = time.monotonic() + 15
    while process.poll() is None and time.monotonic() < deadline:
        targets = browser.call("Target.getTargets").get("targetInfos", [])
        pages = [target for target in targets if target["type"] == "page"]
        for target in pages:
            if not target["url"].startswith(ORIGIN + "/"):
                continue
            if target["targetId"] != target_id:
                attached = browser.call("Target.attachToTarget", {
                    "targetId": target["targetId"], "flatten": True,
                })
                session = attached.get("sessionId")
                if session is None:
                    continue
                target_id = target["targetId"]
            result = browser.call("Runtime.evaluate", {
                "expression": """location.origin === 'https://music.youtube.com' &&
                    window.ytcfg?.get('LOGGED_IN') && ({
                        account: ytcfg.get('SESSION_INDEX') ?? 0,
                        page: ytcfg.get('DELEGATED_SESSION_ID')
                    })""",
                "returnByValue": True,
            }, session)
            identity = result.get("result", {}).get("value")
            if not isinstance(identity, dict):
                continue
            cookies = browser.call("Network.getCookies", {
                "urls": [ORIGIN + "/youtubei/v1/browse"],
            }, session).get("cookies", [])
            if not any(cookie["name"] == "__Secure-3PAPISID" for cookie in cookies):
                continue
            headers = {
                "Authorization": "SAPISIDHASH",
                "Origin": ORIGIN,
                "X-Goog-AuthUser": str(identity["account"]),
                "Cookie": "; ".join(f'{cookie["name"]}={cookie["value"]}' for cookie in cookies),
            }
            if identity.get("page"):
                headers["X-Goog-PageId"] = identity["page"]
            return headers
        time.sleep(0.5)
    raise EOFError


def login(on_closed):
    browser = os.getenv("LINTEL_LOGIN_BROWSER") or next(
        (path for name in ("chromium", "google-chrome", "helium") if (path := shutil.which(name))),
        None,
    )
    if not browser:
        raise FileNotFoundError("Set LINTEL_LOGIN_BROWSER to a Chromium browser")
    headers = browser_session(browser, on_closed)
    client = YTMusic(headers)
    playlists = library_playlists(client)
    visitor = client.base_headers.get("X-Goog-Visitor-Id")
    if visitor:
        headers["X-Goog-Visitor-Id"] = visitor
    if CACHE.is_dir():
        shutil.rmtree(CACHE)
    with tempfile.TemporaryDirectory(dir=CONFIG) as temp:
        saved = Path(temp) / "session.json"
        saved.write_text(json.dumps(headers))
        saved.replace(SESSION)
    return [playlist(item) for item in playlists if item.get("playlistId")]


def playlist(item):
    thumbnails = item.get("thumbnails") or []
    return {
        "ID": item["playlistId"],
        "Name": item.get("title") or "Playlist",
        "Art": thumbnails[-1]["url"] if thumbnails else "",
    }


def library_playlists(client, limit=None):
    playlists = client.get_library_playlists(limit=limit)
    if not playlists:
        # ytmusicapi also returns an empty list for YouTube's signed-out page.
        try:
            client.get_account_info()
        except KeyError as error:
            raise ExpiredSession from error
    return playlists


def logout():
    SESSION.unlink(missing_ok=True)
    OAUTH.unlink(missing_ok=True)
    LIBRARY.unlink(missing_ok=True)
    if CACHE.is_dir():
        shutil.rmtree(CACHE)


def cache_file(playlist_id):
    return CACHE / hashlib.sha256(playlist_id.encode()).hexdigest()


def cached_playlist(path):
    try:
        if time.time() - path.stat().st_mtime < 86400:
            return json.loads(path.read_text())
    except (OSError, ValueError):
        pass
    return None


def save_json(path, value):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=path.parent) as temp:
        saved = Path(temp) / "data.json"
        saved.write_text(json.dumps(value, ensure_ascii=False))
        saved.replace(path)


def cache_library(playlists):
    try:
        save_json(LIBRARY, playlists)
    except OSError:
        pass


def song(item):
    thumbnails = item.get("thumbnails") or []
    return {
        "ID": item["videoId"],
        "Title": item.get("title") or "Unknown track",
        "Artist": ", ".join(artist["name"] for artist in item.get("artists") or []),
        "Art": thumbnails[-1]["url"] if thumbnails else "",
        "Seconds": item.get("duration_seconds") or 0,
    }


def fetch_playlist(yt, playlist_id, first_page=False):
    result = yt.get_playlist(playlist_id, limit=1 if first_page else None)
    return {
        "Playlist": {
            **playlist({**result, "playlistId": playlist_id}),
            "Partial": first_page,
        },
        "Songs": [
            {**song(item), "SetVideoId": item.get("setVideoId")}
            for item in result["tracks"]
            if item.get("videoId") and item.get("isAvailable", True)
        ],
    }


def save_playlist(path, reply):
    try:
        save_json(path, reply)
    except OSError:
        pass


def remove_song(yt, playlist_id, video_id):
    path = cache_file(playlist_id)
    cached = cached_playlist(path)
    if cached is not None and remove_entry(yt, playlist_id, video_id, cached):
        save_playlist(path, cached)
        return cached
    fresh = fetch_playlist(yt, playlist_id)
    if not remove_entry(yt, playlist_id, video_id, fresh):
        raise PlaylistEditError("The track is missing or this playlist isn’t editable.")
    save_playlist(path, fresh)
    return fresh


def remove_entry(yt, playlist_id, video_id, reply):
    songs = reply["Songs"]
    for index, entry in enumerate(songs):
        if entry["ID"] != video_id or not entry.get("SetVideoId"):
            continue
        try:
            result = yt.remove_playlist_items(playlist_id, [{
                "videoId": video_id,
                "setVideoId": entry["SetVideoId"],
            }])
        except YTMusicServerError as error:
            if str(error).startswith(("Server returned HTTP 400:", "Server returned HTTP 404:")):
                return False
            raise
        status = result.get("status") if isinstance(result, dict) else result
        if status == "STATUS_SUCCEEDED":
            songs.pop(index)
            return True
        return False
    return False


def kugou_request(method, **params):
    query = urlencode({**params, "ver": 1, "client": "pc"})
    with urlopen(f"https://lyrics.kugou.com/{method}?{query}", timeout=3) as response:
        return json.loads(response.read(1_000_000))


def parse_krc(content):
    encoded = base64.b64decode(content, validate=True)
    if not encoded.startswith(b"krc1"):
        return []
    # KRC wraps a zlib stream with a repeating XOR mask after its four-byte signature.
    mask = b"@Gaw^2tGQ61-\xce\xd2ni"
    packed = bytes(byte ^ mask[index % len(mask)] for index, byte in enumerate(encoded[4:]))
    stream = zlib.decompressobj()
    sheet = stream.decompress(packed, 1_000_000)
    if not stream.eof:
        return []
    lines = []
    for row in sheet.decode("utf-8-sig").splitlines():
        header = re.fullmatch(r"\[(\d+),(\d+)\](.*)", row)
        if not header:
            continue
        start, length = map(int, header.group(1, 2))
        fragments, words, offset = [], [], 0
        for at, fragment in re.findall(r"<(\d+),\d+,-?\d+>([^<]*)", header[3]):
            fragments.append(fragment)
            offset += len(fragment.encode())
            if fragment.strip():
                words.append({"start": (start + int(at)) / 1000, "end_byte": offset})
        text = "".join(fragments)
        if words:
            lines.append({"text": text, "start": start / 1000,
                          "end": (start + length) / 1000, "words": words})
    return lines


def word_lyrics(title, artist, duration):
    if not (title and artist and duration > 0):
        return None

    def normalized(text):
        return "".join(char for char in text.casefold() if char.isalnum())

    try:
        length = round(duration * 1000)
        response = kugou_request("search", man="yes", keyword=f"{artist} - {title}", duration=length)
        matches = [
            item for item in response.get("candidates", [])
            if item.get("krctype") == 2
            and normalized(item["song"]) == normalized(title)
            and normalized(item["singer"]) == normalized(artist)
            and abs(item["duration"] - length) <= 8000
        ]
        match = min(matches, key=lambda item: abs(item["duration"] - length), default=None)
        if match:
            reply = kugou_request("download", id=match["id"], accesskey=match["accesskey"],
                                  fmt="krc", charset="utf8")
            lines = parse_krc(reply.get("content", ""))
            if lines:
                return {"source": "Kugou", "lines": lines}
    except (OSError, ValueError, KeyError, TypeError, zlib.error):
        pass
    return None


def song_lyrics(yt, args):
    if args.title is None:
        lyrics_id = yt.get_watch_playlist(videoId=args.video_id).get("lyrics")
        return yt.get_lyrics(lyrics_id) if lyrics_id else None
    lyrics_id = yt.get_watch_playlist(videoId=args.video_id).get("lyrics")
    if not lyrics_id:
        return None
    try:
        reply = yt.get_lyrics(lyrics_id, timestamps=True)
    except (KeyError, YTMusicServerError):
        reply = None
    if not reply:
        reply = yt.get_lyrics(lyrics_id)
    if not reply:
        return None
    lines = ([{"text": line.text, "start": line.start_time / 1000,
               "end": line.end_time / 1000} for line in reply["lyrics"] if line.text.strip()]
             if reply["hasTimestamps"]
             else [{"text": line} for line in reply["lyrics"].strip().splitlines()])
    return {"source": (reply.get("source") or "YouTube Music").removeprefix("Source: "), "lines": lines}


def handle(yt, args):
    if args.command == "library":
        return [playlist(item) for item in library_playlists(yt)
                if item.get("playlistId")]
    if args.command == "playlist":
        reply = fetch_playlist(yt, args.playlist_id, args.first_page)
        if not args.first_page:
            save_playlist(cache_file(args.playlist_id), reply)
        return reply
    if args.command == "search":
        return [song(item) for item in yt.search(args.query, filter="songs", limit=20)
                if item.get("videoId")]
    if args.command == "lyrics":
        return song_lyrics(yt, args)
    if args.command == "remove":
        return remove_song(yt, args.playlist_id, args.video_id)
    result = yt.add_playlist_items(args.playlist_id, [args.video_id])
    status = result.get("status") if isinstance(result, dict) else result
    if status != "STATUS_SUCCEEDED":
        raise PlaylistEditError("YouTube Music rejected the playlist change.")
    try:
        cache_file(args.playlist_id).unlink(missing_ok=True)
    except OSError:
        pass
    return None


def main():
    parser = argparse.ArgumentParser(prog="lintel music", description="YouTube Music library")
    sub = parser.add_subparsers(dest="command", required=True)
    login_command = sub.add_parser("login", help="Connect YouTube Music in your browser")
    login_command.add_argument("--library", action="store_true", help=argparse.SUPPRESS)
    sub.add_parser("logout", help="Remove the saved YouTube Music session")
    sub.add_parser("library", help="List saved playlists")
    listing = sub.add_parser("playlist", help="List playable tracks in a playlist")
    listing.add_argument("playlist_id")
    listing.add_argument("--prefer-cache", action="store_true", help=argparse.SUPPRESS)
    listing.add_argument("--first-page", action="store_true", help=argparse.SUPPRESS)
    sub.add_parser("search", help="Search for songs").add_argument("query")
    lyrics = sub.add_parser("lyrics", help="Read lyrics for a video ID")
    lyrics.add_argument("video_id")
    lyrics.add_argument("--title", help=argparse.SUPPRESS)
    lyrics.add_argument("--artist", default="", help=argparse.SUPPRESS)
    lyrics.add_argument("--duration", type=float, default=0, help=argparse.SUPPRESS)
    for name, help_text in [("add", "Add a track to a playlist"), ("remove", "Remove a track from a playlist")]:
        command = sub.add_parser(name, help=help_text)
        command.add_argument("playlist_id")
        command.add_argument("video_id")
    args = parser.parse_args()

    os.umask(0o077)
    if args.command not in ("login", "logout") and not (SESSION.is_file() or (CLIENT.is_file() and OAUTH.is_file())):
        print("Sign in to see saved playlists", file=sys.stderr)
        return 3
    CONFIG.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        if args.command == "login":
            playlists = login(lambda: print(flush=True) if args.library else None)
            cache_library(playlists)
            if args.library:
                print(json.dumps(playlists, ensure_ascii=False))
        else:
            if args.command == "lyrics" and args.title is not None:
                words = word_lyrics(args.title, args.artist, args.duration)
                if words:
                    print(json.dumps(words, ensure_ascii=False))
                    return 0
            if args.command == "playlist" and args.prefer_cache:
                cached = cached_playlist(cache_file(args.playlist_id))
                if cached is not None:
                    print(json.dumps(cached, ensure_ascii=False))
                    return 0
            with (CONFIG / "youtube.lock").open("a") as lock:
                reading_browser = SESSION.is_file() and args.command in (
                    "library", "playlist", "search", "lyrics",
                )
                fcntl.flock(lock, fcntl.LOCK_SH if reading_browser else fcntl.LOCK_EX)
                if args.command == "logout":
                    logout()
                    return 0
                client = connect_library()
                result = handle(client, args)
                if args.command == "library":
                    cache_library(result)
                if SESSION.is_file():
                    headers = json.loads(SESSION.read_text())
                    visitor = client.base_headers.get("X-Goog-Visitor-Id")
                    if visitor and not headers.get("X-Goog-Visitor-Id"):
                        headers["X-Goog-Visitor-Id"] = visitor
                        try:
                            save_json(SESSION, headers)
                        except OSError:
                            pass
                print(json.dumps(result, ensure_ascii=False))
    except (SignInError, PlaylistEditError) as error:
        print(error, file=sys.stderr)
        return 1
    except ExpiredSession:
        LIBRARY.unlink(missing_ok=True)
        print("YouTube Music sign-in expired", file=sys.stderr)
        return 4
    except (KeyboardInterrupt, EOFError):
        print("Sign-in cancelled", file=sys.stderr)
        return 1
    except FileNotFoundError:
        print("Could not start the sign-in browser.", file=sys.stderr)
        return 1
    except Exception:
        print("YouTube Music request failed. Check your connection or sign in again.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
