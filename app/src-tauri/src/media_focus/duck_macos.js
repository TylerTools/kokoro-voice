// Temporarily adjust playing Spotify and Music sessions. The parent holds
// stdin open for the lease; EOF restores their original app volumes.
// Never changes the Mac output device or master volume.
ObjC.import('Foundation');

function writeLine(line) {
  const data = $.NSString.stringWithString(`${line}\n`).dataUsingEncoding($.NSUTF8StringEncoding);
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
}

function duckLevel() {
  const raw = $.NSProcessInfo.processInfo.environment.objectForKey('HEREWORD_DUCK_LEVEL');
  const value = raw.isNil() ? NaN : Number(ObjC.unwrap(raw));
  return Number.isFinite(value) ? Math.min(0.95, Math.max(0.40, value)) : 0.80;
}

function owned(entry, volume) {
  // Spotify may report an older ramp step for a short time. Every volume we
  // wrote is between these bounds; a value outside them is a manual change.
  return typeof volume === 'number' &&
    volume >= Math.min(entry.original, entry.target) - 2 &&
    volume <= Math.max(entry.original, entry.target) + 2;
}

function fade(entry, from, to) {
  for (let step = 1; step <= 8; step++) {
    if (!entry.app.running() || !owned(entry, entry.app.soundVolume())) return false;
    const progress = step / 8;
    const smooth = progress * progress * (3 - 2 * progress);
    entry.app.soundVolume = Math.round(from + (to - from) * smooth);
    delay(0.06);
  }
  return true;
}

function playingApp(bundleID, level) {
  try {
    const app = Application(bundleID);
    if (!app.running() || app.playerState() !== 'playing') return null;
    const original = app.soundVolume();
    if (typeof original !== 'number' || original <= 0 || original > 100) return null;
    return {
      app,
      player: bundleID === 'com.spotify.client' ? 'spotify' : 'music',
      original,
      target: Math.max(1, Math.round(original * level)),
    };
  } catch (_) {
    return null;
  }
}

function restore(entry) {
  try {
    if (!entry.app.running()) return { player: entry.player, original: entry.original, restored: false, reason: 'closed' };
    // Let Spotify settle the last fade-down write before reading its volume.
    delay(0.12);
    let actual = entry.app.soundVolume();
    if (!owned(entry, actual)) {
      return { player: entry.player, original: entry.original, actual, restored: false, reason: 'volume-changed' };
    }
    if (!fade(entry, actual, entry.original)) {
      actual = entry.app.soundVolume();
      return { player: entry.player, original: entry.original, actual, restored: false, reason: 'fade-interrupted' };
    }
    // Spotify's setter can read back one point lower than requested. Correct
    // against its measured value instead of repeating the same wrong write.
    let command = entry.original;
    for (let attempt = 0; attempt < 10; attempt++) {
      actual = entry.app.soundVolume();
      if (!owned(entry, actual)) {
        return { player: entry.player, original: entry.original, actual, restored: false, reason: 'volume-changed' };
      }
      if (actual === entry.original) {
        delay(0.18);
        if (entry.app.soundVolume() === entry.original) {
          return { player: entry.player, original: entry.original, actual: entry.original, restored: true };
        }
      }
      if (Math.abs(entry.original - actual) <= 3) {
        command = Math.max(0, Math.min(100, command + entry.original - actual));
      } else {
        command = entry.original;
      }
      entry.app.soundVolume = command;
      delay(0.14);
    }
    actual = entry.app.soundVolume();
    return { player: entry.player, original: entry.original, actual, restored: actual === entry.original };
  } catch (_) {
    return { player: entry.player, original: entry.original, restored: false, reason: 'control-error' };
  }
}

function run() {
  const changed = [];
  const level = duckLevel();
  let ducked = 0;
  try {
    for (const bundleID of ['com.spotify.client', 'com.apple.Music']) {
      const entry = playingApp(bundleID, level);
      if (!entry) continue;
      changed.push(entry);
      // Give the parent the original volume before the first adjustment, so
      // it can recover if this helper is killed during a fade.
      writeLine(`BASELINE ${JSON.stringify({ player: entry.player, original: entry.original, target: entry.target })}`);
      if (fade(entry, entry.original, entry.target)) ducked++;
    }
    writeLine(`READY ${ducked}`);
    $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  } finally {
    const results = changed.map(restore);
    writeLine(`RESTORE ${JSON.stringify(results)}`);
  }
}

run();
