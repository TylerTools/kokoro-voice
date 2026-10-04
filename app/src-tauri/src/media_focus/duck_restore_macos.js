// One bounded recovery attempt after the lease helper fails. Only restore an
// app volume still inside the range HereWord wrote; leave manual changes alone.
ObjC.import('Foundation');

const raw = $.NSProcessInfo.processInfo.environment.objectForKey('HEREWORD_DUCK_RECOVERY');
const entries = raw.isNil() ? [] : JSON.parse(ObjC.unwrap(raw));
const results = [];
for (const entry of entries) {
  const bundleID = entry.player === 'spotify' ? 'com.spotify.client' :
    entry.player === 'music' ? 'com.apple.Music' : null;
  if (!bundleID || !Number.isInteger(entry.original) || !Number.isInteger(entry.target) ||
      entry.original < 1 || entry.original > 100 || entry.target < 1 || entry.target > 100) {
    continue;
  }
  try {
    const app = Application(bundleID);
    if (!app.running()) {
      results.push({ player: entry.player, restored: false, reason: 'closed' });
      continue;
    }
    const actual = app.soundVolume();
    if (typeof actual !== 'number' ||
        actual < Math.min(entry.original, entry.target) - 2 ||
        actual > Math.max(entry.original, entry.target) + 2) {
      results.push({ player: entry.player, restored: false, reason: 'volume-changed' });
      continue;
    }
    let command = entry.original;
    let restored = (() => {
      for (let attempt = 0; attempt < 6; attempt++) {
        app.soundVolume = command;
        delay(0.14);
        const observed = app.soundVolume();
        if (observed === entry.original) return true;
        if (typeof observed !== 'number' ||
            observed < Math.min(entry.original, entry.target) - 2 ||
            observed > Math.max(entry.original, entry.target) + 2) return false;
        command = Math.max(0, Math.min(100, command + entry.original - observed));
      }
      return false;
    })();
    results.push({ player: entry.player, restored });
  } catch (_) {
    results.push({ player: entry.player, restored: false, reason: 'control-error' });
  }
}
JSON.stringify(results);
