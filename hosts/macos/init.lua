-- ═══════════════════════════════════════════════════════════════════════════
-- Kokoro read-aloud: recorder + hotkey + draggable mini player
--
-- KVM FACTS (measured 2026-07-26 through Deskflow, keyspy.log):
--   Shift+Z  arrives as  key=z mods=[]     <- Shift is CONSUMED
--   Cmd+V    arrives as  key=v mods=[cmd]  <- Cmd SURVIVES
-- Deskflow turns shift+<letter> into a CHARACTER and drops the shift flag, so
-- {shift}+<letter> can never match. Modifier-only chords and function keys are
-- unaffected. Fn never crosses the link at all.
-- Hence the hotkey is RECORDED, not configured -- see Recorder below.
--
-- RESERVED -- do not bind: Fn, and Ctrl+Shift. Leave these free for whatever
-- other reading/dictation tool you are migrating away from.
--
-- PLAYBACK RUNS IN-PROCESS (hs.sound), NOT via afplay. Measured: `afplay`
-- costs ~0.93s of dead air per invocation; hs.sound costs ~0.21s, and
-- preloading the next clip while the current one plays removes even that.
-- Do not "simplify" this back to shelling out to afplay.
-- ═══════════════════════════════════════════════════════════════════════════

-- Explicitly OFF, and it must stay an explicit call. This is a PERSISTED
-- preference (HSAppleScriptEnabledKey in org.hammerspoon.Hammerspoon), not a
-- per-run flag -- simply deleting the old `hs.allowAppleScript(true)` left it
-- enabled across restarts. Verified: after removing the line, osascript could
-- still execute Lua here. Asserting false on every load is self-healing.
hs.allowAppleScript(false)

-- DO NOT re-enable hs.allowAppleScript(true). Hammerspoon holds the
-- Accessibility grant (it can synthesize keystrokes and read any window), and
-- that setting exposes its Lua interpreter over Apple Events -- verified:
--   osascript -e 'tell application "Hammerspoon" to execute lua code "..."'
-- runs arbitrary Lua, and hs.execute() from there is arbitrary shell, as you,
-- with no TCC prompt. Any local process that can send an Apple Event (a rogue
-- npm postinstall, a downloaded .scpt) inherits the whole grant. Nothing in
-- this config uses the AppleScript API.

local SPEAK = os.getenv("HOME") .. "/tts/kokoro-service/client/speak.py"
local PYTHON = "/usr/bin/python3"
-- dictate.py needs sounddevice/soundfile, so it runs in the service venv,
-- unlike speak.py which is deliberately stdlib-only.
local VENV_PYTHON = os.getenv("HOME") .. "/tts/kokoro-service/.venv/bin/python"
local DICTATE = os.getenv("HOME") .. "/tts/kokoro-service/client/dictate.py"
local LOG = os.getenv("HOME") .. "/.hammerspoon/hotkey-debug.log"

local function log(msg)
    local fh = io.open(LOG, "a")
    if fh then fh:write(os.date("%H:%M:%S ") .. msg .. "\n"); fh:close() end
end

local function notify(msg)
    hs.notify.new({ title = "Kokoro TTS", informativeText = msg }):send()
end

local MOD_ORDER = { "ctrl", "alt", "shift", "cmd", "fn" }
local MOD_GLYPH = { ctrl = "⌃", alt = "⌥", shift = "⇧", cmd = "⌘", fn = "fn" }

local function modList(flags)
    local out = {}
    for _, m in ipairs(MOD_ORDER) do if flags[m] then out[#out + 1] = m end end
    return out
end

local function describe(b)
    if not b then return "none" end
    local s = ""
    for _, m in ipairs(b.mods or {}) do s = s .. (MOD_GLYPH[m] or m) end
    if b.kind == "chord" then return s .. " (tap)" end
    return s .. string.upper(b.key or "?")
end

-- ═══ audio engine — gapless, in-process ═══════════════════════════════════
Audio = { queue = {}, sounds = {}, current = nil, idx = 0, playing = false, paused = false }

function Audio:reset()
    -- Drop the completion callbacks BEFORE stopping. hs.sound fires the
    -- callback on stop() as well as on natural end, and that callback calls
    -- advance() -- which would run against a half-cleared queue and pop the
    -- player back up showing the replay glyph right after a stop.
    for _, s in pairs(self.sounds) do pcall(function() s:setCallback(nil) end) end
    if self.current then
        pcall(function() self.current:setCallback(nil) end)
        pcall(function() self.current:stop() end)
    end
    for _, p in ipairs(self.queue) do os.remove(p) end
    self.queue, self.sounds, self.current = {}, {}, nil
    self.idx, self.playing, self.paused = 0, false, false
    self.finished = false
end

--- Preload as soon as a path arrives so the next clip is ready to fire.
function Audio:enqueue(path)
    self.queue[#self.queue + 1] = path
    local s = hs.sound.getByFile(path)
    if s then
        s:setCallback(function() Audio:advance() end)
        self.sounds[#self.queue] = s
    end
    if not self.playing then self:advance() end
end

function Audio:advance()
    local nextIdx = self.idx + 1
    local s = self.sounds[nextIdx]

    if not s then
        -- Nothing ready. CRITICAL: do NOT consume the index here. An earlier
        -- version incremented idx unconditionally, so when a clip finished
        -- before the next one had been synthesized, the index overshot and the
        -- late-arriving chunk was skipped forever -- audio stopped after the
        -- first sentence. Leave idx alone; enqueue() will call us again.
        self.playing = false
        if ProducerDone and nextIdx > #self.queue then
            self:reset()
            self.finished = true -- reset() clears it, so set it after
            Player:showFinished()
        else
            -- Buffer underrun: synthesis hasn't caught up. Go back to the
            -- spinner so the pause icon never sits there looking stalled --
            -- the user should always be able to tell work is still happening.
            Player:showLoading()
        end
        return
    end

    -- Only now retire the clip we just played.
    if self.idx > 0 and self.queue[self.idx] then
        os.remove(self.queue[self.idx])
        self.sounds[self.idx] = nil
    end

    self.idx = nextIdx
    self.current = s
    self.playing, self.paused = true, false
    Player:showPlaying()
    s:play()
end

function Audio:toggle()
    if not self.current then return "idle" end
    if self.paused then
        self.current:resume(); self.paused = false; return "playing"
    end
    self.current:pause(); self.paused = true; return "paused"
end

--- Stop means STOP: tear down the source, not just the output.
---
--- Killing the sound alone was not enough and looked like the button was dead.
--- The producer keeps synthesizing after a stop, and every CHUNK line it emits
--- reaches Audio:enqueue(), which ends with
---     if not self.playing then self:advance() end
--- reset() had just set playing=false, so the next chunk RESTARTED the playback
--- that was stopped -- repeatedly, until synthesis finished the whole passage.
--- That is why it only ever misbehaved on an in-progress read.
function Audio:stop()
    -- Bump FIRST: the generation guard makes any callback still in flight from
    -- this producer a no-op, including the ones that fire during teardown.
    SpeakGen = SpeakGen + 1
    if Producer then
        Producer:terminate()
        Producer = nil
    end
    ProducerDone = true
    self:reset()
    Player:hide()
    log("STOP: producer terminated, queue cleared")
end

-- ═══ mini player (draggable) ══════════════════════════════════════════════
Player = { canvas = nil, dragTap = nil }

local W, H = 132, 40
local BG     = { red = 0.12, green = 0.12, blue = 0.13, alpha = 0.94 }
local FG     = { white = 1, alpha = 0.95 }
local ACCENT = { red = 0.35, green = 0.72, blue = 1.0, alpha = 1.0 }
local SPIN   = { "◜", "◝", "◞", "◟" }

-- How long the finished/replay control lingers before fading away, and how
-- long the fade itself takes. Tweak FINISH_LINGER to taste.
FINISH_LINGER = 20   -- seconds
FADE_SECONDS  = 1.5

local function defaultPos()
    local f = hs.screen.primaryScreen():frame()
    return { x = f.x + f.w - W - 18, y = f.y + 12 }
end

local function savedPos()
    local p = hs.settings.get("kokoroPlayerPos")
    if not p or not p.x then return defaultPos() end
    -- keep it on-screen if the display arrangement changed
    local f = hs.screen.primaryScreen():frame()
    if p.x < f.x - W or p.x > f.x + f.w or p.y < f.y - H or p.y > f.y + f.h then
        return defaultPos()
    end
    return p
end

function Player:build()
    if self.canvas then return end
    local p = savedPos()
    local c = hs.canvas.new({ x = p.x, y = p.y, w = W, h = H })
    c:appendElements(
        { type = "rectangle", action = "fill", fillColor = BG,
          roundedRectRadii = { xRadius = 10, yRadius = 10 },
          trackMouseDown = true, id = "bg" },
        { type = "text", text = "❙❙", id = "toggle", textColor = ACCENT,
          textSize = 15, textAlignment = "center",
          frame = { x = 0, y = 10, w = W / 2, h = 22 }, trackMouseDown = true },
        { type = "text", text = "■", id = "stop", textColor = FG,
          textSize = 15, textAlignment = "center",
          frame = { x = W / 2, y = 9, w = W / 2, h = 22 }, trackMouseDown = true },
        { type = "segments", action = "stroke",
          strokeColor = { white = 1, alpha = 0.18 }, strokeWidth = 1,
          coordinates = { { x = W / 2, y = 10 }, { x = W / 2, y = H - 10 } } }
    )
    c:level(hs.canvas.windowLevels.overlay)
    c:behavior(hs.canvas.windowBehaviors.canJoinAllSpaces)
    c:clickActivating(false)

    c:mouseCallback(function(_, event, id)
        if event ~= "mouseDown" then return end
        if id == "toggle" then
            Player:toggle()
        elseif id == "stop" then
            Audio:stop()
        elseif id == "bg" then
            Player:beginDrag() -- drag by the body; buttons stay clickable
        end
    end)
    c:canvasMouseEvents(true, true, false, false)
    self.canvas = c
end

--- Drag the panel anywhere; position persists across restarts.
function Player:beginDrag()
    self:cancelFade()
    if Audio.finished then self:scheduleFade() end
    if self.dragTap then self.dragTap:stop() end
    local startMouse = hs.mouse.absolutePosition()
    local f = self.canvas:frame()
    local origin = { x = f.x, y = f.y }

    self.dragTap = hs.eventtap.new({
        hs.eventtap.event.types.leftMouseDragged,
        hs.eventtap.event.types.leftMouseUp,
    }, function(e)
        local t = e:getType()
        if t == hs.eventtap.event.types.leftMouseUp then
            self.dragTap:stop(); self.dragTap = nil
            local nf = self.canvas:frame()
            hs.settings.set("kokoroPlayerPos", { x = nf.x, y = nf.y })
            return false
        end
        local m = hs.mouse.absolutePosition()
        self.canvas:topLeft({
            x = origin.x + (m.x - startMouse.x),
            y = origin.y + (m.y - startMouse.y),
        })
        return false
    end)
    self.dragTap:start()
end

function Player:startSpinner()
    self:stopSpinner()
    local i = 1
    self.canvas[2].textColor = { white = 1, alpha = 0.55 }
    self.canvas[2].text = SPIN[1]
    self.spinTimer = hs.timer.doEvery(0.12, function()
        if not self.canvas then return end
        i = (i % #SPIN) + 1
        self.canvas[2].text = SPIN[i]
    end)
end

function Player:stopSpinner()
    if self.spinTimer then self.spinTimer:stop(); self.spinTimer = nil end
end

function Player:showLoading()
    self:build(); self:cancelFade(); self.canvas:show(); self:startSpinner()
end

function Player:showPlaying()
    self:build(); self:cancelFade(); self:stopSpinner()
    self.canvas[2].textColor = ACCENT
    self.canvas[2].text = "❙❙"
    self.canvas:show()
end

--- Mic is live.
function Player:showRecording()
    self:build(); self:cancelFade(); self:stopSpinner()
    self.canvas[2].textColor = { red = 1, green = 0.35, blue = 0.35, alpha = 1 }
    self.canvas[2].text = "\u{25CF}"   -- filled dot
    self.canvas:show()
end

--- Playback is over: stay on screen offering a replay rather than vanishing,
--- then fade out so it doesn't sit there forever covering something.
function Player:showFinished()
    self:build(); self:stopSpinner()
    self.canvas[2].textColor = ACCENT
    self.canvas[2].text = "\u{27F3}"   -- replay
    self.canvas:alpha(1.0)
    self.canvas:show()
    self:scheduleFade()
end

--- Linger with the replay control, then fade. Any new activity cancels it.
function Player:scheduleFade()
    self:cancelFade()
    self.fadeTimer = hs.timer.doAfter(FINISH_LINGER, function()
        if Audio.finished and self.canvas then
            self.canvas:hide(FADE_SECONDS) -- canvas:hide() fades when given a duration
        end
        self.fadeTimer = nil
    end)
end

function Player:cancelFade()
    if self.fadeTimer then self.fadeTimer:stop(); self.fadeTimer = nil end
    if self.canvas then self.canvas:alpha(1.0) end
end

function Player:hide()
    self:stopSpinner(); self:cancelFade()
    if self.canvas then self.canvas:hide() end
end

function Player:toggle()
    if Audio.finished then Replay(); return end
    local state = Audio:toggle()
    if self.canvas then
        self.canvas[2].text = (state == "paused") and "▶" or "❙❙"
    end
    if state == "idle" then self:hide() end
end

function Player:resetPosition()
    hs.settings.set("kokoroPlayerPos", defaultPos())
    if self.canvas then self.canvas:topLeft(defaultPos()) end
end

-- ═══ speak ════════════════════════════════════════════════════════════════
--- Copy the current selection, then hand it to `callback`.
---
--- Asynchronous on purpose. The old version posted Cmd+C and then blocked the
--- Hammerspoon main thread with usleep(180ms) waiting for the pasteboard.
---
--- The real defect it hid: a MODIFIER-CHORD hotkey fires on release, but the
--- keys are often still physically down at that instant. A synthetic Cmd+C
--- posted while Shift is held becomes Shift+Cmd+C -- which is not copy. So
--- chord bindings always reported "no selection" while a plain F5 worked fine.
--- We therefore wait for the physical modifiers to actually clear first.
local function getSelection(callback)
    local saved = hs.pasteboard.getContents()
    local tries = 0

    local function attempt()
        local m = hs.eventtap.checkKeyboardModifiers()
        local held = m.cmd or m.shift or m.alt or m.ctrl or m.fn
        tries = tries + 1
        if held and tries < 25 then          -- up to ~500ms
            hs.timer.doAfter(0.02, attempt)
            return
        end

        hs.pasteboard.clearContents()
        -- NOTE: no 0 delay here. keyStroke(..., 0) sends key-down and key-up
        -- back to back with no gap, which many apps miss entirely -- that made
        -- the copy succeed only intermittently. The default delay is reliable.
        hs.eventtap.keyStroke({ "cmd" }, "c")

        hs.timer.doAfter(0.20, function()
            local text = hs.pasteboard.getContents()
            hs.timer.doAfter(0.35, function()
                -- Only put the old contents back if OUR copied text is still
                -- on the pasteboard. Unguarded, this restore could land after a
                -- dictation had already written its transcript here and wipe
                -- it -- the dictation path guards the same way.
                if saved and hs.pasteboard.getContents() == text then
                    hs.pasteboard.setContents(saved)
                end
            end)
            callback(text)
        end)
    end

    attempt()
end

Producer, ProducerDone = nil, true

-- Every read gets a generation number, and both producer callbacks check it
-- before touching shared state.
--
-- WHY: hs.task:terminate() is ASYNCHRONOUS. When a read interrupts another, the
-- superseded producer's exit callback fires *after* SpeakText has already set
-- ProducerDone=false and reassigned Producer. That stale callback then sets
-- ProducerDone=true and Producer=nil on the NEW read, and two things break:
--   1. Audio:advance() sees ProducerDone with an empty queue and declares the
--      new passage finished after its first chunk -- audio stops early.
--   2. Producer=nil means the next read cannot terminate the still-running
--      orphan, whose CHUNK lines keep landing in the new read's queue.
-- Interrupting is now the common path, so this has to be right.
SpeakGen = 0

--- Speak an explicit string. Kept separate from selection-grabbing so the
--- audio path can be exercised without a live text selection.
function SpeakText(text)
    LastText = text
    Audio.finished = false
    if not text or text:gsub("%s", "") == "" then
        Player:hide(); notify("Nothing to speak."); log("  empty text"); return
    end

    -- Bump the generation BEFORE terminating, not after: terminate() can drive
    -- callbacks synchronously, and any that fire in between would still match
    -- the old generation and be treated as live.
    SpeakGen = SpeakGen + 1
    local gen = SpeakGen
    if Producer then Producer:terminate() end
    Audio:reset()
    ProducerDone = false
    log("  synthesizing " .. #text .. " chars")

    local buf = ""
    Producer = hs.task.new(PYTHON, function(code)
        if gen ~= SpeakGen then return end   -- superseded: not our state to touch
        ProducerDone = true
        Producer = nil
        if code ~= 0 then Player:hide() end
    end, function(_, stdout)
        if gen ~= SpeakGen then
            -- Superseded mid-stream. Delete any clips this orphan already wrote
            -- (nothing else knows about them, so they would sit in the state dir
            -- forever) and stop consuming its output.
            for p in (stdout or ""):gmatch("CHUNK ([^\n]+)") do os.remove(p) end
            return false
        end
        buf = buf .. (stdout or "")
        while true do
            local line, rest = buf:match("^([^\n]*)\n(.*)$")
            if not line then break end
            buf = rest
            local path = line:match("^CHUNK (.+)$")
            if path then
                Audio:enqueue(path)
            elseif line:match("^ERROR ") then
                notify(line:gsub("^ERROR ", ""))
                Player:hide()
            end
        end
        return true
    end, { SPEAK, "--stdin", "--emit-paths", "--speed", tostring(Speed()) })

    -- ORDER MATTERS, AND closeInput() IS MANDATORY.
    -- speak.py --stdin blocks in sys.stdin.read() until EOF. hs.task:setInput()
    -- alone never closes the pipe, so without closeInput() the process hangs
    -- FOREVER and the hotkey looks like it is "just very slow". This was the
    -- actual cause of the perceived slowness -- verified: exit code never
    -- arrived without it, exit=0 with it.
    Producer:setInput(text)
    Producer:start()
    Producer:closeInput()
end

local function speakSelection()
    log("TRIGGER fired")
    Player:showLoading() -- instant feedback before any work

    getSelection(function(text)
        if not text or text:gsub("%s", "") == "" then
            Player:hide()
            notify("No text selected — highlight something first")
            log("  no selection"); return
        end
        SpeakText(text)
    end)
end

--- Re-speak the last passage. Re-synthesizes (~0.7s to first audio) rather
--- than caching WAVs, which keeps temp files from accumulating.
function Replay()
    if not LastText or LastText == "" then Player:hide(); return end
    Player:showLoading()
    SpeakText(LastText)
end

--- Second-press semantics.
---
--- The hotkey has to serve two intents that look identical at the keyboard:
--- "pause this" and "forget that, read what I just selected". So when audio is
--- already going we grab the selection first and let the CONTENT decide:
---   different text -> you selected something new; interrupt and read it
---   same text, or nothing selected -> you meant pause/resume
---
--- Cost: a second press now waits for the copy round-trip (~250ms) before
--- pausing, because we cannot know which intent it was until we look. If you
--- want instant pause, the mini player's ❙❙ button is still immediate -- it
--- does not go through this path.
function Trigger()
    -- Never fight the microphone: a synthetic Cmd+C mid-dictation would land
    -- in whatever app is focused, and both paths contend for the pasteboard.
    -- Dictation is declared further down; it is a global, so it exists by the
    -- time this ever runs. The nil check is belt-and-braces for load order.
    if Dictation and Dictation.recording then
        log("TRIGGER ignored (dictation recording)")
        return
    end

    if not (Audio.playing or Audio.paused) then
        speakSelection()
        return
    end

    log("TRIGGER while playing -- checking for new selection")
    getSelection(function(text)
        local hasText = text and text:gsub("%s", "") ~= ""
        if hasText and text ~= LastText then
            log("  new selection -> interrupting")
            Player:showLoading()
            SpeakText(text)
        else
            log("  same/no selection -> pause toggle")
            Player:toggle()
        end
    end)
end

-- ═══ dictation ════════════════════════════════════════════════════════════
Dictation = { task = nil, recording = false }

function Dictation:toggle()
    if self.recording then self:stop() else self:start() end
end

-- Hard ceiling. A missed key-release once left a recorder running for 3
-- minutes before MAX_SECONDS caught it; this stops that from depending on the
-- Python side alone.
DICTATE_MAX = 120

-- How long a transcript is allowed to sit on the clipboard before the previous
-- contents are put back. The clipboard is the fallback for when keystrokes miss
-- their target, so this needs to outlast "did that work? let me paste it" --
-- but not the session.
CLIP_RESTORE = 45

function Dictation:start()
    if self.recording then return end   -- key auto-repeat must not restart it
    self.startedAt = hs.timer.secondsSinceEpoch()
    if self.watchdog then self.watchdog:stop() end
    self.watchdog = hs.timer.doAfter(DICTATE_MAX, function()
        if Dictation.recording then
            log("  watchdog: forcing stop after " .. DICTATE_MAX .. "s")
            notify("Dictation stopped after " .. DICTATE_MAX .. "s")
            Dictation:stop()
        end
    end)
    if self.task then self.task:terminate() end
    self.recording = true
    self:duck()            -- pause any read-aloud before the mic opens
    Player:showLoading()   -- spinner until the mic is actually open
    log("DICTATE start")

    local buf = ""
    self.task = hs.task.new(VENV_PYTHON, function(code)
        Dictation.recording = false
        Dictation.task = nil
        if code ~= 0 then Player:hide() end
        Dictation:unduck()      -- backstop: never leave playback stuck paused
    end, function(_, stdout)
        buf = buf .. (stdout or "")
        while true do
            local line, rest = buf:match("^([^\n]*)\n(.*)$")
            if not line then break end
            buf = rest
            if line == "RECORDING" then
                Player:showRecording()
            elseif line:match("^TRANSCRIBING") then
                Player:showLoading()
            elseif line:match("^TEXT ") then
                local text = line:sub(6)
                log("  dictated " .. #text .. " chars")
                Player:hide()
                -- Clipboard FIRST, then type. If focus wasn't in a text field
                -- the keystrokes go nowhere, and without this the transcript
                -- would be lost outright -- so the clipboard is the safety net,
                -- and it must be set even if typing fails.
                --
                -- But it is a TIMED safety net, not a permanent one. Dictation
                -- is exactly where sensitive text shows up, and the clipboard
                -- is readable by every running app, persisted by clipboard-
                -- history managers, and synced off-device by Universal
                -- Clipboard. So restore what was there before after
                -- CLIP_RESTORE seconds -- long enough to Cmd-V manually if the
                -- typing missed, short enough that a transcript is not left
                -- sitting on the pasteboard for the rest of the session.
                -- The TTS selection path does the same thing (see getSelection).
                local prior = hs.pasteboard.getContents()
                hs.pasteboard.setContents(text)
                hs.eventtap.keyStrokes(text)
                Dictation:unduck()
                if Dictation.clipTimer then Dictation.clipTimer:stop() end
                Dictation.clipTimer = hs.timer.doAfter(CLIP_RESTORE, function()
                    -- Only clear if OUR text is still up; if the user copied
                    -- something else meanwhile, leave their clipboard alone.
                    if hs.pasteboard.getContents() == text then
                        hs.pasteboard.setContents(prior or "")
                        log("  clipboard restored after " .. CLIP_RESTORE .. "s")
                    end
                    Dictation.clipTimer = nil
                end)
            elseif line:match("^ERROR ") then
                Player:hide()
                -- already told them to hold the key; don't double-notify
                if not Dictation.tooShort then
                    notify(line:gsub("^ERROR ", ""))
                end
                log("  " .. line)
            end
        end
        return true
    end, { DICTATE, "--record" })
    self.task:start()
end

-- Below this, the user tapped rather than held: there is no usable audio and
-- the generic "no audio captured" error reads like a malfunction.
DICTATE_MIN_HOLD = 0.45

--- Pause read-aloud while the mic is live, then put it back.
--- Two reasons: the user should not have to listen while talking, and the mic
--- would otherwise record the TTS output and feed it into the transcript.
function Dictation:duck()
    self.ducked = false
    if Audio.current and Audio.playing and not Audio.paused then
        Audio.current:pause()
        Audio.paused = true
        self.ducked = true
        log("  ducked playback for dictation")
    end
end

function Dictation:unduck()
    if not self.ducked then return end
    self.ducked = false
    -- Only resume if playback is still the thing that was interrupted; the user
    -- may have hit stop, or a new read may have started, in the meantime.
    if Audio.current and Audio.paused then
        Audio.current:resume()
        Audio.paused = false
        Player:showPlaying()
        log("  resumed playback after dictation")
    end
end

--- Abandon an in-flight recording: no transcription, no notification.
--- Used when the chord turns out to be a normal shortcut (Shift+Cmd+P etc).
function Dictation:cancel()
    if not self.recording then return end
    if self.watchdog then self.watchdog:stop(); self.watchdog = nil end
    self.recording = false
    self.tooShort = true          -- suppress the downstream "no audio" notice
    if self.task then self.task:terminate(); self.task = nil end
    Player:hide()
    self:unduck()
    log("  chord was a shortcut — recording abandoned")
end

function Dictation:stop()
    if not self.recording then return end
    local heldFor = self.startedAt and (hs.timer.secondsSinceEpoch() - self.startedAt) or 99
    if heldFor < DICTATE_MIN_HOLD then
        log(string.format("  too short (%.2fs) — hold to talk", heldFor))
        notify("Hold the key while you speak, then release")
        self.tooShort = true
    else
        self.tooShort = false
    end
    if self.watchdog then self.watchdog:stop(); self.watchdog = nil end
    self.recording = false
    log("DICTATE stop")
    hs.task.new(VENV_PYTHON, nil, { DICTATE, "--stop" }):start()
end

-- ═══ binding engine ═══════════════════════════════════════════════════════
local hotkeyHandle, chordWatcher, chordKeyWatcher
local armed, armedAt = false, 0

local function clearBinding()
    if hotkeyHandle then hotkeyHandle:delete(); hotkeyHandle = nil end
    if chordWatcher then chordWatcher:stop(); chordWatcher = nil end
    if chordKeyWatcher then chordKeyWatcher:stop(); chordKeyWatcher = nil end
end

function sameSet(a, b)
    if #a ~= #b then return false end
    local seen = {}
    for _, v in ipairs(a) do seen[v] = true end
    for _, v in ipairs(b) do if not seen[v] then return false end end
    return true
end

function ApplyBinding(b)
    clearBinding()
    if not b then return end
    if b.kind == "key" then
        hotkeyHandle = hs.hotkey.bind(b.mods or {}, b.key, Trigger)
        log("bound key: " .. describe(b)); return
    end
    local want = b.mods or {}
    chordWatcher = hs.eventtap.new({ hs.eventtap.event.types.flagsChanged }, function(e)
        local mods = modList(e:getFlags())
        if #mods > 0 and sameSet(mods, want) then
            armed, armedAt = true, hs.timer.secondsSinceEpoch()
        elseif #mods == 0 then
            if armed and (hs.timer.secondsSinceEpoch() - armedAt) < 1.2 then
                armed = false; Trigger()
            end
            armed = false
        end
        return false
    end)
    chordWatcher:start()
    chordKeyWatcher = hs.eventtap.new({ hs.eventtap.event.types.keyDown }, function()
        armed = false; return false
    end)
    chordKeyWatcher:start()
    log("bound chord: " .. describe(b))
end

function CurrentBinding() return hs.settings.get("kokoroHotkey") end
function CurrentDictateBinding() return hs.settings.get("kokoroDictateHotkey") end

local dictHandle, dictChord, dictChordKey
local dArmed, dArmedAt = false, 0
-- Set when a chord turns out to be an ordinary shortcut. Blocks re-arming
-- until EVERY modifier is released: otherwise letting go of the letter in
-- Shift+Cmd+P leaves Shift+Cmd still held, which matches the chord again and
-- immediately starts a second recording.
local dBlocked = false

function ApplyDictateBinding(b)
    if dictHandle then dictHandle:delete(); dictHandle = nil end
    if dictChord then dictChord:stop(); dictChord = nil end
    if dictChordKey then dictChordKey:stop(); dictChordKey = nil end
    if not b then return end

    -- PUSH TO TALK. Hold the key to record, release to transcribe.
    -- Toggle semantics failed in practice: a start with no matching stop left
    -- the recorder running invisibly. With press/release there is no way to
    -- end up recording without holding the key.
    if b.kind == "key" then
        dictHandle = hs.hotkey.bind(b.mods or {}, b.key,
            function() Dictation:start() end,   -- pressed
            function() Dictation:stop()  end)   -- released
        log("bound dictate key (push-to-talk): " .. describe(b)); return
    end
    -- Modifier chord, also push-to-talk: recording runs while the modifiers
    -- are held and stops the moment they are released. No hold timeout, so a
    -- long dictation is fine.
    local want = b.mods or {}
    dictChord = hs.eventtap.new({ hs.eventtap.event.types.flagsChanged }, function(e)
        local mods = modList(e:getFlags())
        if #mods == 0 then
            dBlocked = false          -- everything released; safe to arm again
            if dArmed then
                dArmed = false
                Dictation:stop()
            end
        elseif sameSet(mods, want) then
            if not dArmed and not dBlocked then
                dArmed = true
                Dictation:start()
            end
        end
        return false
    end)
    dictChord:start()

    -- CRITICAL: a real keypress while the chord is held means this was an
    -- ordinary shortcut (Shift+Cmd+P, Shift+Cmd+Z, Shift+Cmd+4 ...), not a
    -- push-to-talk gesture. Without this, EVERY such shortcut started the
    -- microphone and produced a "too short" error on release. The read-aloud
    -- chord always had this guard; push-to-talk dropped it.
    dictChordKey = hs.eventtap.new({ hs.eventtap.event.types.keyDown }, function()
        if dArmed then
            dArmed = false
            dBlocked = true           -- do not re-arm on the way out
            Dictation:cancel()
        end
        return false
    end)
    dictChordKey:start()

    log("bound dictate chord (push-to-talk): " .. describe(b))
end

-- ═══ recorder ═════════════════════════════════════════════════════════════
Recorder = { canvas = nil, tap = nil, flagTap = nil, sawFlags = nil, keyHit = false }
local RW, RH = 380, 118

function Recorder:close()
    if self.tap then self.tap:stop(); self.tap = nil end
    if self.flagTap then self.flagTap:stop(); self.flagTap = nil end
    if self.canvas then self.canvas:delete(); self.canvas = nil end
end

function Recorder:setLine(a, b)
    if not self.canvas then return end
    self.canvas[3].text = a
    if b then self.canvas[4].text = b end
end

function Recorder:save(b)
    local slot = self.target or "kokoroHotkey"
    hs.settings.set(slot, b)
    if slot == "kokoroDictateHotkey" then ApplyDictateBinding(b) else ApplyBinding(b) end
    self:setLine(describe(b), "saved")
    Settings:refresh()
    hs.timer.doAfter(1.1, function() Recorder:close() end)
    notify("Read-aloud hotkey set to " .. describe(b))
end

function Recorder:start(target)
    self:close(); self.keyHit = false; self.sawFlags = nil
    self.target = target or "kokoroHotkey"
    local f = hs.screen.primaryScreen():frame()
    local c = hs.canvas.new({ x = f.x + (f.w - RW) / 2, y = f.y + (f.h - RH) / 3, w = RW, h = RH })
    c:appendElements(
        { type = "rectangle", action = "fill",
          fillColor = { red = 0.10, green = 0.10, blue = 0.12, alpha = 0.97 },
          roundedRectRadii = { xRadius = 14, yRadius = 14 } },
        { type = "text", text = (self.target == "kokoroDictateHotkey")
              and "Press the keys you want for dictation"
              or  "Press the keys you want for read-aloud",
          textColor = { white = 1, alpha = 0.75 }, textSize = 13,
          textAlignment = "center", frame = { x = 0, y = 14, w = RW, h = 20 } },
        { type = "text", text = "…", textColor = ACCENT, textSize = 26,
          textAlignment = "center", frame = { x = 0, y = 40, w = RW, h = 36 } },
        { type = "text", text = "modifiers alone are fine — release to record · esc cancels",
          textColor = { white = 1, alpha = 0.45 }, textSize = 11,
          textAlignment = "center", frame = { x = 0, y = 84, w = RW, h = 20 } }
    )
    c:level(hs.canvas.windowLevels.modalPanel)
    c:behavior(hs.canvas.windowBehaviors.canJoinAllSpaces)
    c:show()
    self.canvas = c

    self.tap = hs.eventtap.new({ hs.eventtap.event.types.keyDown }, function(e)
        local name = hs.keycodes.map[e:getKeyCode()]
        if name == "escape" then
            self:setLine("cancelled", "")
            hs.timer.doAfter(0.6, function() Recorder:close() end)
            return true
        end
        self.keyHit = true
        self:save({ kind = "key", mods = modList(e:getFlags()), key = name })
        return true
    end)

    self.flagTap = hs.eventtap.new({ hs.eventtap.event.types.flagsChanged }, function(e)
        local mods = modList(e:getFlags())
        if #mods > 0 then
            if not self.sawFlags or #mods >= #self.sawFlags then self.sawFlags = mods end
            self:setLine(describe({ kind = "chord", mods = mods }), "release to record")
        else
            if self.sawFlags and not self.keyHit then
                self:save({ kind = "chord", mods = self.sawFlags })
            end
            self.sawFlags = nil
        end
        return false
    end)

    self.tap:start(); self.flagTap:start()
end

-- ═══ settings panel ══════════════════════════════════════════════════════
Settings = { canvas = nil }

local SPEEDS = { 0.75, 1.0, 1.25, 1.5, 1.75, 2.0 }
local SW, SH = 430, 292

function Speed()
    return hs.settings.get("kokoroSpeed") or 1.0
end

function SetSpeed(v)
    hs.settings.set("kokoroSpeed", v)
    Settings:refresh()
    -- Re-speak at the new rate so the change is audible immediately rather
    -- than silently applying to some future read.
    if LastText and (Audio.playing or Audio.paused or Audio.finished) then
        Replay()
    end
end

function Settings:close()
    if self.canvas then self.canvas:delete(); self.canvas = nil end
end

function Settings:refresh()
    if not self.canvas then return end
    self.canvas["hotkeyVal"].text = describe(CurrentBinding())
    self.canvas["dictVal"].text = describe(CurrentDictateBinding())
    local cur = Speed()
    for i, v in ipairs(SPEEDS) do
        local on = math.abs(v - cur) < 0.001
        self.canvas["chipBg" .. i].fillColor =
            on and ACCENT or { white = 1, alpha = 0.10 }
        self.canvas["chip" .. i].textColor =
            on and { white = 1 } or { white = 1, alpha = 0.75 }
    end
end

function Settings:open()
    self:close()
    local f = hs.screen.primaryScreen():frame()
    local c = hs.canvas.new({
        x = f.x + (f.w - SW) / 2, y = f.y + (f.h - SH) / 3, w = SW, h = SH,
    })

    c:appendElements(
        { type = "rectangle", action = "fill",
          fillColor = { red = 0.10, green = 0.10, blue = 0.12, alpha = 0.98 },
          roundedRectRadii = { xRadius = 14, yRadius = 14 },
          trackMouseDown = true, id = "panelBg" },
        { type = "text", text = "Kokoro read-aloud", textColor = { white = 1, alpha = 0.9 },
          textSize = 15, frame = { x = 20, y = 16, w = SW - 40, h = 22 } },
        { type = "text", text = "✕", id = "close", textColor = { white = 1, alpha = 0.6 },
          textSize = 15, textAlignment = "right",
          frame = { x = SW - 44, y = 15, w = 26, h = 24 }, trackMouseDown = true },

        -- hotkey row
        { type = "text", text = "Hotkey", textColor = { white = 1, alpha = 0.55 },
          textSize = 12, frame = { x = 20, y = 56, w = 120, h = 18 } },
        { type = "text", text = "…", id = "hotkeyVal", textColor = ACCENT,
          textSize = 18, frame = { x = 20, y = 74, w = 220, h = 26 } },
        { type = "rectangle", action = "fill", id = "recordBg",
          fillColor = { white = 1, alpha = 0.12 },
          roundedRectRadii = { xRadius = 7, yRadius = 7 },
          frame = { x = SW - 130, y = 72, w = 110, h = 30 }, trackMouseDown = true },
        { type = "text", text = "Record…", id = "record", textColor = { white = 1, alpha = 0.9 },
          textSize = 13, textAlignment = "center",
          frame = { x = SW - 130, y = 78, w = 110, h = 22 }, trackMouseDown = true },

        -- dictation hotkey row
        { type = "text", text = "Dictation hotkey", textColor = { white = 1, alpha = 0.55 },
          textSize = 12, frame = { x = 20, y = 112, w = 160, h = 18 } },
        { type = "text", text = "…", id = "dictVal", textColor = ACCENT,
          textSize = 18, frame = { x = 20, y = 130, w = 220, h = 26 } },
        { type = "rectangle", action = "fill", id = "dictRecBg",
          fillColor = { white = 1, alpha = 0.12 },
          roundedRectRadii = { xRadius = 7, yRadius = 7 },
          frame = { x = SW - 130, y = 128, w = 110, h = 30 }, trackMouseDown = true },
        { type = "text", text = "Record…", id = "dictRec", textColor = { white = 1, alpha = 0.9 },
          textSize = 13, textAlignment = "center",
          frame = { x = SW - 130, y = 134, w = 110, h = 22 }, trackMouseDown = true },

        -- speed row
        { type = "text", text = "Playback speed", textColor = { white = 1, alpha = 0.55 },
          textSize = 12, frame = { x = 20, y = 176, w = 200, h = 18 } }
    )

    -- speed chips
    local chipW, gap = 60, 8
    local x0 = 20
    for i, v in ipairs(SPEEDS) do
        local x = x0 + (i - 1) * (chipW + gap)
        c:appendElements(
            { type = "rectangle", action = "fill", id = "chipBg" .. i,
              fillColor = { white = 1, alpha = 0.10 },
              roundedRectRadii = { xRadius = 7, yRadius = 7 },
              frame = { x = x, y = 200, w = chipW, h = 32 }, trackMouseDown = true },
            { type = "text", text = string.format("%gx", v), id = "chip" .. i,
              textColor = { white = 1, alpha = 0.75 }, textSize = 13,
              textAlignment = "center",
              frame = { x = x, y = 150, w = chipW, h = 22 }, trackMouseDown = true }
        )
    end

    c:appendElements(
        { type = "text",
          text = "Changing speed re-reads the current passage.",
          textColor = { white = 1, alpha = 0.4 }, textSize = 11,
          frame = { x = 20, y = 250, w = SW - 40, h = 20 } }
    )

    c:level(hs.canvas.windowLevels.modalPanel)
    c:behavior(hs.canvas.windowBehaviors.canJoinAllSpaces)

    c:mouseCallback(function(_, event, id)
        if event ~= "mouseDown" then return end
        if id == "close" then
            Settings:close()
        elseif id == "record" or id == "recordBg" then
            Recorder:start("kokoroHotkey")
        elseif id == "dictRec" or id == "dictRecBg" then
            Recorder:start("kokoroDictateHotkey")
        else
            local n = tostring(id):match("^chip[Bg]*(%d+)$")
            if n then SetSpeed(SPEEDS[tonumber(n)]) end
        end
    end)
    c:canvasMouseEvents(true, true, false, false)
    c:show()
    self.canvas = c
    self:refresh()
end

-- ═══ menu bar ═════════════════════════════════════════════════════════════
menu = hs.menubar.new()
if menu then
    menu:setTitle("🔈")
    menu:setMenu(function()
        return {
            { title = "Read selection", fn = function() Trigger() end },
            { title = Dictation.recording and "Stop dictation" or "Dictate",
              fn = function() Dictation:toggle() end },
            { title = "Stop", fn = function() Audio:stop() end },
            { title = "Replay last", fn = function() Replay() end,
              disabled = (LastText == nil) },
            { title = "-" },
            { title = "Hotkey: " .. describe(CurrentBinding()), disabled = true },
            { title = "Record read hotkey…", fn = function() Recorder:start("kokoroHotkey") end },
            { title = "Dictate hotkey: " .. describe(CurrentDictateBinding()), disabled = true },
            { title = "Record dictate hotkey…",
              fn = function() Recorder:start("kokoroDictateHotkey") end },
            { title = string.format("Speed: %gx", Speed()), disabled = true },
            { title = "Settings…", fn = function() Settings:open() end },
            { title = "-" },
            { title = "Reset player position", fn = function() Player:resetPosition() end },
        }
    end)
end

-- ═══ startup ══════════════════════════════════════════════════════════════
if not CurrentBinding() then
    hs.settings.set("kokoroHotkey", { kind = "key", mods = {}, key = "f5" })
end
if not CurrentDictateBinding() then
    hs.settings.set("kokoroDictateHotkey", { kind = "key", mods = {}, key = "f6" })
end
ApplyBinding(CurrentBinding())
ApplyDictateBinding(CurrentDictateBinding())

hs.autoLaunch(true)
hs.menuIcon(true)

log("=== loaded; accessibility=" .. tostring(hs.accessibilityState())
    .. "; hotkey=" .. describe(CurrentBinding()) .. " ===")
hs.alert.show("Kokoro ready — " .. describe(CurrentBinding()) .. " · drag the player to move it")
