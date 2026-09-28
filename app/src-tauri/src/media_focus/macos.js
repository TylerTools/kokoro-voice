// Owns a bounded Now Playing query/command, never app automation or volume.
// Keep metadata private and require unchanged identity before each command.
ObjC.import('Foundation');

function state() {
  const request = $.NSClassFromString('MRNowPlayingRequest');
  const path = request.localNowPlayingPlayerPath;
  const item = request.localNowPlayingItem;
  if (path.isNil() || item.isNil()) return null;
  const info = item.nowPlayingInfo;
  if (info.isNil()) return null;
  const value = key => {
    const v = info.valueForKey(key);
    return v.isNil() ? null : ObjC.unwrap(v);
  };
  const player = ObjC.unwrap(path.client.bundleIdentifier);
  const rate = value('kMRMediaRemoteNowPlayingInfoPlaybackRate');
  if (!player || typeof rate !== 'number') return null;
  const title = value('kMRMediaRemoteNowPlayingInfoTitle');
  const identifier = value('kMRMediaRemoteNowPlayingInfoUniqueIdentifier');
  if (!identifier && !title) return null;
  const track = JSON.stringify([
    String(identifier || '').slice(0, 256), String(title || '').slice(0, 256),
    String(value('kMRMediaRemoteNowPlayingInfoArtist') || '').slice(0, 256),
    value('kMRMediaRemoteNowPlayingInfoDuration'),
  ]);
  return { player, track, playing: rate > 0 };
}

function run(argv) {
  $.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/').load;
  const current = state();
  if (argv[0] === 'get') return JSON.stringify(current);
  const expected = JSON.parse(argv[1]);
  if (!current || !expected || current.player !== expected.player ||
      current.track !== expected.track || current.playing !== expected.playing) return 'false';
  const playing = argv[0] === 'play';
  if (!playing && argv[0] !== 'pause') return 'false';
  const controller = $.NSClassFromString('MRNowPlayingController').localRouteController;
  controller.sendCommandOptionsCompletion(playing ? 0 : 1, $.NSDictionary.alloc.init, null);
  // Sending a command alone is not evidence the player accepted it.
  for (let n = 0; n < 8; n++) {
    delay(0.05);
    const next = state();
    if (!next || next.player !== current.player || next.track !== current.track) return 'false';
    if (next.playing === playing) return 'true';
  }
  return 'false';
}
