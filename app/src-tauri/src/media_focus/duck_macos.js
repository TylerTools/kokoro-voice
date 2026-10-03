// Owns temporary volume changes for playing Spotify and Music sessions only.
// Standard input is the parent-lifetime lease: EOF fades back after a crash.
// Never changes the Mac output device or its master volume.
ObjC.import('Foundation');

function writeReady() {
  const data = $.NSString.stringWithString('READY\n').dataUsingEncoding($.NSUTF8StringEncoding);
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
}

function fade(entry, from, to) {
  let expected = from;
  for (let step = 1; step <= 8; step++) {
    if (!entry.app.running()) return false;
    const actual = entry.app.soundVolume();
    if (typeof actual !== 'number' || Math.abs(actual - expected) > 2) return false;
    const progress = step / 8;
    const smooth = progress * progress * (3 - 2 * progress);
    const next = Math.round(from + (to - from) * smooth);
    entry.app.soundVolume = next;
    expected = next;
    entry.applied = next;
    delay(0.04);
  }
  return true;
}

function playingApp(bundleID) {
  try {
    const app = Application(bundleID);
    if (!app.running() || app.playerState() !== 'playing') return null;
    const volume = app.soundVolume();
    if (typeof volume !== 'number' || volume <= 0 || volume > 100) return null;
    return { app, original: volume, applied: volume };
  } catch (_) {
    return null;
  }
}

function run() {
  const changed = [];
  try {
    for (const bundleID of ['com.spotify.client', 'com.apple.Music']) {
      const entry = playingApp(bundleID);
      if (!entry) continue;
      changed.push(entry);
      const target = Math.max(1, Math.round(entry.original * 0.25));
      fade(entry, entry.original, target);
    }
    writeReady();
    $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  } finally {
    for (const entry of changed) {
      try {
        if (entry.app.running() && Math.abs(entry.app.soundVolume() - entry.applied) <= 2) {
          fade(entry, entry.applied, entry.original);
        }
      } catch (_) { /* A closed player needs no volume change. */ }
    }
  }
}

run();
