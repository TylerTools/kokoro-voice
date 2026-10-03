// Owns temporary volume changes for playing Spotify and Music sessions only.
// Standard input is the parent-lifetime lease: EOF fades back after a crash.
// Never changes the Mac output device or its master volume.
ObjC.import('Foundation');

function writeReady() {
  const data = $.NSString.stringWithString('READY\n').dataUsingEncoding($.NSUTF8StringEncoding);
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
}

function fade(entry, from, to, restoring = false) {
  let expected = from;
  let previous = from;
  for (let step = 1; step <= 8; step++) {
    if (!entry.app.running()) return false;
    const actual = entry.app.soundVolume();
    // Spotify may report the previous ramp step while a volume write settles.
    // Allow that lag on the way back up, while still honoring a larger manual change.
    const tolerance = restoring ? Math.max(3, Math.abs(expected - previous) + 2) : 2;
    if (typeof actual !== 'number' || Math.abs(actual - expected) > tolerance) return false;
    const progress = step / 8;
    const smooth = progress * progress * (3 - 2 * progress);
    const next = Math.round(from + (to - from) * smooth);
    entry.app.soundVolume = next;
    previous = expected;
    expected = next;
    entry.applied = next;
    delay(0.06);
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

function duckLevel() {
  const raw = $.NSProcessInfo.processInfo.environment.objectForKey('HEREWORD_DUCK_LEVEL');
  const value = raw.isNil() ? NaN : Number(ObjC.unwrap(raw));
  return Number.isFinite(value) ? Math.min(0.95, Math.max(0.40, value)) : 0.80;
}

function run() {
  const changed = [];
  const level = duckLevel();
  try {
    for (const bundleID of ['com.spotify.client', 'com.apple.Music']) {
      const entry = playingApp(bundleID);
      if (!entry) continue;
      changed.push(entry);
      const target = Math.max(1, Math.round(entry.original * level));
      fade(entry, entry.original, target);
    }
    writeReady();
    $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  } finally {
    for (const entry of changed) {
      try {
        if (entry.app.running() && Math.abs(entry.app.soundVolume() - entry.applied) <= 2) {
          fade(entry, entry.applied, entry.original, true);
        }
      } catch (_) { /* A closed player needs no volume change. */ }
    }
  }
}

run();
