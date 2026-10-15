-- CatosTV: TV-style player controls for the Catfyn player (mpv): OK
-- brings the controls, OK again pauses, like a normal TV player. Loaded by
-- the shell with --script.
--
--   controls hidden:  OK / Up / Down  show the controls (Pause focused)
--                     Left / Right    scrub 10 s back / forward
--                     Back            stop
--   controls shown:   Left / Right    move over the buttons
--                     OK              press the button (Pause first)
--                     Up              focus the seek bar: Left / Right scrub
--                                     (10 s, 60 s when held), Down returns
--                     Back            hide the controls
--   The bar hides itself 4 s after the last key while playing; it stays
--   while paused.
--   Subtitles / Audio open a list drawn here: Up/Down move, OK
--   picks ("Off" first for subtitles), Back closes. External subtitle
--   files get their server names from --script-opts=catfyn_osd-subs=a;b.
--   Next episode: the shell's player loop decides everything (when
--   the credits start, the countdown, "Are you still watching?") and tells
--   this script what to draw with `script-message-to catfyn_osd
--   catfyn-card next LABEL LEFT [THUMB W H] | count LEFT | still LABEL
--   [THUMB W H] | hide`. While the card shows, Left / Right move between its two
--   buttons, OK presses one, Back cancels; the choice goes back as
--   `script-message catfyn-card-result play|cancel|continue|stop`. Every
--   key also sends `catfyn-input` (the "still watching" counter). New
--   subtitle names for the next episode arrive as `catfyn-subs A;B`.
--
-- Drawn as ASS on a 1920x1080 canvas (scaled by mpv), accent from
-- --script-opts=catfyn_osd-accent=RRGGBB.

local mp = require "mp"
local options = require "mp.options"
local assdraw = require "mp.assdraw"

local opts = { accent = "7c6cf0", subs = "" }
options.read_options(opts, "catfyn_osd")

-- Names for external subtitle tracks, in the order they were added.
local sub_names = {}
for name in string.gmatch(opts.subs, "[^;]+") do
    sub_names[#sub_names + 1] = name
end

local menu = nil -- { kind = "sub"|"audio", items = {...}, index = n }
local card = nil -- { kind = "next"|"still", label, left, focus, thumb }

local W, H = 1920, 1080
local shown = false
local focus = 2 -- Pause
local on_bar = false -- the seek bar has focus
local scrubs = 0 -- consecutive scrubs while held: step grows
local hide_timer = nil
local tick_timer = nil

local function seek(secs)
    mp.commandv("seek", secs, "relative")
end

local open_menu -- defined below (the buttons table refers to it)

local buttons = {
    { label = "−10 s",     act = function() seek(-10) end },
    { label = "Pause",     act = function() mp.commandv("cycle", "pause") end, toggles = true },
    { label = "+10 s",     act = function() seek(10) end },
    { label = "Subtitles", act = function() open_menu("sub") end },
    { label = "Audio",     act = function() open_menu("audio") end },
    { label = "Stop",      act = function() mp.commandv("quit") end },
}

-- "RRGGBB" → ASS "&HBBGGRR&"
local function ass_rgb(hex)
    hex = hex:gsub("#", "")
    if #hex ~= 6 then hex = "7c6cf0" end
    return string.format("&H%s%s%s&", hex:sub(5, 6), hex:sub(3, 4), hex:sub(1, 2))
end
local ACCENT = ass_rgb(opts.accent)

local function clock(secs)
    if not secs or secs < 0 then secs = 0 end
    secs = math.floor(secs + 0.5)
    local h = math.floor(secs / 3600)
    local m = math.floor(secs % 3600 / 60)
    local s = secs % 60
    if h > 0 then
        return string.format("%d:%02d:%02d", h, m, s)
    end
    return string.format("%d:%02d", m, s)
end

local function rect(a, x, y, w, h, r)
    -- Rounded rectangle (r = corner radius) as an ASS drawing.
    r = math.min(r, w / 2, h / 2)
    a:move_to(x + r, y)
    a:line_to(x + w - r, y)
    a:bezier_curve(x + w, y, x + w, y, x + w, y + r)
    a:line_to(x + w, y + h - r)
    a:bezier_curve(x + w, y + h, x + w, y + h, x + w - r, y + h)
    a:line_to(x + r, y + h)
    a:bezier_curve(x, y + h, x, y + h, x, y + h - r)
    a:line_to(x, y + r)
    a:bezier_curve(x, y, x, y, x + r, y)
end

local function text_width(label, size)
    -- Rough: the font is proportional, 0.56 em per character is close for
    -- the labels used here.
    return #label * size * 0.56
end

local render_menu, render_card -- below

-- Everything drawn goes through here, so the Next episode card is drawn
-- over whatever else shows.
local function set_osd(text)
    if card then
        local c = assdraw.ass_new()
        render_card(c)
        text = (text ~= "" and (text .. "\n") or "") .. c.text
    end
    mp.set_osd_ass(W, H, text)
end

local function render()
    if not shown and not menu then
        set_osd("")
        return
    end
    if not shown then
        local a = assdraw.ass_new()
        render_menu(a)
        set_osd(a.text)
        return
    end
    local pos = mp.get_property_number("time-pos", 0)
    local dur = mp.get_property_number("duration", 0)
    local paused = mp.get_property_native("pause", false)
    local title = mp.get_property("media-title", "")
    buttons[2].label = paused and "Play" or "Pause"

    local a = assdraw.ass_new()
    -- Dark veil along the bottom.
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H000000&\\1a&H50&}")
    a:draw_start()
    a:rect_cw(0, 820, W, H)
    a:draw_stop()
    -- Title.
    a:new_event()
    local clean = title:gsub("[{}]", "")
    a:append("{\\an1\\pos(120,900)\\bord0\\shad0\\fs40\\b1\\1c&HFFFFFF&\\1a&H10&}" .. clean)
    -- Progress track and fill.
    local tx, ty, tw, th = 120, 930, 1680, 8
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&HFFFFFF&\\1a&HB0&}")
    a:draw_start()
    rect(a, tx, ty, tw, th, 4)
    a:draw_stop()
    local frac = dur > 0 and math.max(0, math.min(1, pos / dur)) or 0
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c" .. ACCENT .. "\\1a&H00&}")
    a:draw_start()
    rect(a, tx, ty, math.max(8, tw * frac), th, 4)
    a:draw_stop()
    -- Knob (bigger, with an accent ring, while the bar has focus).
    local kr = on_bar and 14 or 10
    if on_bar then
        a:new_event()
        a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c" .. ACCENT .. "\\1a&H60&}")
        a:draw_start()
        a:round_rect_cw(tx + tw * frac - 22, ty - 18, tx + tw * frac + 22, ty + th + 18, 22)
        a:draw_stop()
    end
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&HFFFFFF&\\1a&H00&}")
    a:draw_start()
    a:round_rect_cw(tx + tw * frac - kr, ty + th / 2 - kr, tx + tw * frac + kr, ty + th / 2 + kr, kr)
    a:draw_stop()
    -- Times.
    a:new_event()
    a:append(string.format("{\\an7\\pos(120,950)\\bord0\\shad0\\fs28\\1c&HFFFFFF&\\1a&H30&}%s", clock(pos)))
    a:new_event()
    a:append(string.format("{\\an9\\pos(1800,950)\\bord0\\shad0\\fs28\\1c&HFFFFFF&\\1a&H30&}%s", clock(dur)))
    -- Buttons, centred.
    local size, pad, gap, bh = 30, 44, 18, 62
    local widths, total = {}, 0
    for i, b in ipairs(buttons) do
        widths[i] = text_width(b.label, size) + pad
        total = total + widths[i]
    end
    total = total + gap * (#buttons - 1)
    local x = (W - total) / 2
    local by = 992
    for i, b in ipairs(buttons) do
        local focused = i == focus and not on_bar
        a:new_event()
        if focused then
            a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c" .. ACCENT .. "\\1a&H00&}")
        else
            a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&HFFFFFF&\\1a&HD8&}")
        end
        a:draw_start()
        rect(a, x, by, widths[i], bh, bh / 2)
        a:draw_stop()
        a:new_event()
        a:append(string.format("{\\an5\\pos(%d,%d)\\bord0\\shad0\\fs%d\\b%d\\1c&H%s&\\1a&H00&}%s",
            math.floor(x + widths[i] / 2), math.floor(by + bh / 2), size, focused and 1 or 0,
            focused and "101018" or "FFFFFF", b.label))
        x = x + widths[i] + gap
    end
    if menu then render_menu(a) end
    set_osd(a.text)
end

-- ---------------------------------------------------------------- menus

local function track_label(t, external_n)
    local label = t.title
    if (not label or label == "") and t.external and sub_names[external_n] then
        label = sub_names[external_n]
    end
    if not label or label == "" then
        label = t.lang and t.lang ~= "" and t.lang or ("Track " .. tostring(t.id))
    elseif t.lang and t.lang ~= "" and not label:lower():find(t.lang:lower(), 1, true) then
        label = label .. " (" .. t.lang .. ")"
    end
    if t.forced then label = label .. " · forced" end
    return label
end

local function build_menu(kind)
    local items = {}
    local tracks = mp.get_property_native("track-list", {})
    local current = mp.get_property(kind == "sub" and "sid" or "aid", "no")
    if kind == "sub" then
        items[1] = { label = "Off", id = "no", selected = current == "no" }
    end
    local external_n = 0
    for _, t in ipairs(tracks) do
        if (kind == "sub" and t.type == "sub") or (kind == "audio" and t.type == "audio") then
            if t.external then external_n = external_n + 1 end
            items[#items + 1] = {
                label = track_label(t, external_n),
                id = tostring(t.id),
                selected = t.selected == true,
            }
        end
    end
    local index = 1
    for i, it in ipairs(items) do
        if it.selected then index = i end
    end
    return { kind = kind, items = items, index = index }
end

open_menu = function(kind)
    menu = build_menu(kind)
    if #menu.items == 0 then
        mp.osd_message(kind == "sub" and "No subtitles in this video" or "No audio tracks", 1.5)
        menu = nil
        return
    end
    render()
end

local function menu_pick()
    local it = menu.items[menu.index]
    if not it then menu = nil; render(); return end
    if menu.kind == "sub" then
        mp.set_property("sid", it.id)
        if it.id ~= "no" then mp.set_property("sub-visibility", "yes") end
        mp.osd_message(it.id == "no" and "Subtitles off" or ("Subtitles: " .. it.label), 1.5)
    else
        mp.set_property("aid", it.id)
        mp.osd_message("Audio: " .. it.label, 1.5)
    end
    menu = nil
    render()
end

-- The list panel on the right (drawn over the bar).
render_menu = function(a)
    local px, py, pw = 1300, 140, 540
    local rh, visible = 66, 9
    local n = #menu.items
    local first = math.max(1, math.min(menu.index - visible + 1, n - visible + 1))
    if menu.index < first then first = menu.index end
    local shown_n = math.min(visible, n - first + 1)
    local ph = 96 + shown_n * rh + 24
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H101018&\\1a&H28&}")
    a:draw_start()
    rect(a, px, py, pw, ph, 22)
    a:draw_stop()
    a:new_event()
    a:append(string.format("{\\an7\\pos(%d,%d)\\bord0\\shad0\\fs34\\b1\\1c&HFFFFFF&\\1a&H00&}%s",
        px + 32, py + 28, menu.kind == "sub" and "Subtitles" or "Audio"))
    for row = 0, shown_n - 1 do
        local i = first + row
        local it = menu.items[i]
        local y = py + 96 + row * rh
        local focused = i == menu.index
        if focused then
            a:new_event()
            a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c" .. ACCENT .. "\\1a&H00&}")
            a:draw_start()
            rect(a, px + 16, y, pw - 32, rh - 8, 14)
            a:draw_stop()
        end
        if it.selected then
            a:new_event()
            a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H" .. (focused and "101018" or "FFFFFF") .. "&\\1a&H00&}")
            a:draw_start()
            a:round_rect_cw(px + 38, y + (rh - 8) / 2 - 7, px + 52, y + (rh - 8) / 2 + 7, 7)
            a:draw_stop()
        end
        local label = it.label:gsub("[{}]", "")
        if #label > 34 then label = label:sub(1, 33) .. "…" end
        a:new_event()
        a:append(string.format("{\\an4\\pos(%d,%d)\\bord0\\shad0\\fs28\\b%d\\1c&H%s&\\1a&H00&}%s",
            px + 70, math.floor(y + (rh - 8) / 2), focused and 1 or 0,
            focused and "101018" or "FFFFFF", label))
    end
    if n > visible then
        a:new_event()
        a:append(string.format("{\\an9\\pos(%d,%d)\\bord0\\shad0\\fs22\\1c&HFFFFFF&\\1a&H60&}%d / %d",
            px + pw - 28, py + 40, menu.index, n))
    end
end

-- ------------------------------------------------------- next episode

-- The card sits bottom right (above the control bar when it shows).
local CW, CH = 720, 200
local TW, TH = 256, 144

local function card_box()
    local x = W - 56 - CW
    local y = H - 56 - CH
    if shown then y = y - 250 end
    return x, y
end

local function clean(t)
    t = (t or ""):gsub("[{}\\]", "")
    return t
end

-- Characters, not bytes ("·" is two bytes); cut at a character boundary.
local function fit(t, size, width)
    local max = math.floor(width / (size * 0.45))
    local n = utf8 and utf8.len(t) or #t
    if n and n > max then
        local cut = utf8 and utf8.offset(t, max) or max
        t = t:sub(1, cut - 1) .. "…"
    end
    return t
end

local function pill(a, x, y, w, h, label, focused)
    a:new_event()
    if focused then
        a:append("{\\an7\\pos(0,0)\\bord1.5\\shad0\\1c" .. ACCENT .. "\\1a&H00&\\3c&HFFFFFF&\\3a&H70&}")
    else
        a:append("{\\an7\\pos(0,0)\\bord1\\shad0\\1c&HFFFFFF&\\1a&HE6&\\3c&HFFFFFF&\\3a&HC8&}")
    end
    a:draw_start()
    rect(a, x, y, w, h, h / 2)
    a:draw_stop()
    a:new_event()
    a:append(string.format("{\\an5\\pos(%d,%d)\\bord0\\shad0\\fs26\\b%d\\1c&HFFFFFF&\\1a&H%s&}%s",
        x + w / 2, y + h / 2, focused and 1 or 0, focused and "00" or "30", label))
end

local thumb_at = nil -- where the thumbnail overlay was put (x, y)

local function place_thumb(x, y)
    if not card or not card.thumb then
        if thumb_at then mp.commandv("overlay-remove", 7); thumb_at = nil end
        return
    end
    local dims = mp.get_property_native("osd-dimensions") or {}
    local sx = (dims.w or W) / W
    local sy = (dims.h or H) / H
    local px, py = math.floor(x * sx), math.floor(y * sy)
    if thumb_at and thumb_at[1] == px and thumb_at[2] == py then return end
    local t = card.thumb
    mp.commandv("overlay-add", 7, px, py, t.path, 0, "bgra", t.w, t.h, t.w * 4)
    thumb_at = { px, py }
end

render_card = function(a)
    local x, y = card_box()
    -- Dark glass, a thin light edge, the frosted band along the top.
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord1.5\\shad0\\1c&H241814&\\1a&H38&\\3c&HFFFFFF&\\3a&HB4&}")
    a:draw_start()
    rect(a, x, y, CW, CH, 28)
    a:draw_stop()
    -- Frost: faint layers from the top edge that add up to a soft fade.
    for k = 1, 4 do
        a:new_event()
        a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&HFFFFFF&\\1a&HFA&}")
        a:draw_start()
        rect(a, x + 1, y + 1, CW - 2, 22 * k, 27)
        a:draw_stop()
    end
    -- Thumbnail frame (the picture is an overlay placed on top of it).
    local tx, ty = x + 28, y + 28
    a:new_event()
    a:append("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H000000&\\1a&H60&}")
    a:draw_start()
    rect(a, tx, ty, TW, TH, 14)
    a:draw_stop()
    place_thumb(tx, ty)
    local cx = tx + TW + 28
    local cw = CW - (cx - x) - 28
    local heading, label, b1, b2
    if card.kind == "still" then
        heading = "Are you still watching?"
        b1, b2 = "Continue", "Stop"
    else
        heading = "Next episode in " .. tostring(card.left or 0)
        b1, b2 = "Play now", "Cancel"
    end
    label = fit(clean(card.label), 30, cw)
    a:new_event()
    a:append(string.format("{\\an7\\pos(%d,%d)\\bord0\\shad0\\fs24\\1c&HFFFFFF&\\1a&H50&}%s", cx, y + 26, heading))
    a:new_event()
    a:append(string.format("{\\an7\\pos(%d,%d)\\bord0\\shad0\\fs30\\b1\\1c&HFFFFFF&\\1a&H00&}%s", cx, y + 60, label))
    pill(a, cx, y + 124, 196, 52, b1, card.focus == 1)
    pill(a, cx + 210, y + 124, 160, 52, b2, card.focus == 2)
end

local function card_result(r)
    mp.commandv("script-message", "catfyn-card-result", r)
end

local function card_hide()
    card = nil
    place_thumb(0, 0)
end

mp.register_script_message("catfyn-card", function(cmd, a1, a2, a3, a4, a5)
    if cmd == "next" then
        local thumb = nil
        if a3 and a3 ~= "" and tonumber(a4) and tonumber(a5) then
            thumb = { path = a3, w = tonumber(a4), h = tonumber(a5) }
        end
        card = { kind = "next", label = a1, left = tonumber(a2) or 0, focus = 1, thumb = thumb }
        thumb_at = nil
    elseif cmd == "count" and card then
        card.left = tonumber(a1) or card.left
    elseif cmd == "still" then
        local thumb = card and card.thumb or nil
        if a2 and a2 ~= "" and tonumber(a3) and tonumber(a4) then
            thumb = { path = a2, w = tonumber(a3), h = tonumber(a4) }
        end
        card = { kind = "still", label = a1, focus = 1, thumb = thumb }
        thumb_at = nil
    elseif cmd == "hide" then
        card_hide()
    end
    render()
end)

-- Subtitle names of the next episode's external tracks (same player).
mp.register_script_message("catfyn-subs", function(names)
    sub_names = {}
    for name in string.gmatch(names or "", "[^;]+") do
        sub_names[#sub_names + 1] = name
    end
end)

local function card_ok()
    if card.kind == "still" then
        card_result(card.focus == 1 and "continue" or "stop")
    else
        card_result(card.focus == 1 and "play" or "cancel")
    end
end

local function card_move(dir)
    card.focus = (dir < 0) and 1 or 2
    render()
end

-- Every key counts as "someone is watching".
local function notify()
    mp.commandv("script-message", "catfyn-input")
end

local function stop_timers()
    if hide_timer then hide_timer:kill(); hide_timer = nil end
    if tick_timer then tick_timer:kill(); tick_timer = nil end
end

local function hide()
    shown = false
    menu = nil
    on_bar = false
    scrubs = 0
    stop_timers()
    render()
end

local scrub_timer = nil
local function arm_hide()
    if hide_timer then hide_timer:kill() end
    hide_timer = mp.add_timeout(4, function()
        if menu then arm_hide() return end
        if not mp.get_property_native("pause", false) then hide() end
    end)
    -- A pause in scrubbing ends the held-key acceleration.
    if scrub_timer then scrub_timer:kill() end
    scrub_timer = mp.add_timeout(0.4, function() scrubs = 0 end)
end

local function show()
    shown = true
    render()
    if not tick_timer then
        tick_timer = mp.add_periodic_timer(0.5, render)
    end
    arm_hide()
end

local function on_ok()
    notify()
    if card then card_ok(); return end
    if menu then menu_pick(); return end
    if not shown then
        focus = 2
        show()
        return
    end
    if on_bar then
        mp.commandv("cycle", "pause")
    else
        buttons[focus].act()
    end
    render()
    arm_hide()
end

-- Scrubbing on the bar: 10 s a press, 60 s once it has been held a while.
local function scrub(dir)
    scrubs = scrubs + 1
    seek(dir * (scrubs > 6 and 60 or 10))
    render()
    arm_hide()
end

local function on_left()
    notify()
    if card then card_move(-1); return end
    if menu then return end
    if shown and on_bar then
        scrub(-1)
    elseif shown then
        focus = (focus - 2) % #buttons + 1
        render()
        arm_hide()
    else
        seek(-10)
        mp.commandv("show-progress")
    end
end

local function on_right()
    notify()
    if card then card_move(1); return end
    if menu then return end
    if shown and on_bar then
        scrub(1)
    elseif shown then
        focus = focus % #buttons + 1
        render()
        arm_hide()
    else
        seek(10)
        mp.commandv("show-progress")
    end
end

local function on_up()
    notify()
    if card then return end
    if menu then
        menu.index = (menu.index - 2) % #menu.items + 1
        render()
        return
    end
    if not shown then
        focus = 2
        show()
    elseif not on_bar then
        on_bar = true
        scrubs = 0
        render()
        arm_hide()
    else
        hide()
    end
end

local function on_down()
    notify()
    if card then return end
    if menu then
        menu.index = menu.index % #menu.items + 1
        render()
        return
    end
    if not shown then
        focus = 2
        show()
    elseif on_bar then
        on_bar = false
        render()
        arm_hide()
    else
        arm_hide()
    end
end

local function on_back()
    notify()
    if card then card_result("cancel"); return end
    if menu then menu = nil; render(); return end
    if shown then hide() else mp.commandv("quit") end
end

for _, k in ipairs({ "ENTER", "KP_ENTER", "SPACE" }) do
    mp.add_forced_key_binding(k, "catfyn-ok-" .. k, on_ok)
end
mp.add_forced_key_binding("LEFT", "catfyn-left", on_left, { repeatable = true })
mp.add_forced_key_binding("RIGHT", "catfyn-right", on_right, { repeatable = true })
mp.add_forced_key_binding("UP", "catfyn-up", on_up)
mp.add_forced_key_binding("DOWN", "catfyn-down", on_down)
mp.add_forced_key_binding("ESC", "catfyn-back", on_back)
mp.add_forced_key_binding("BS", "catfyn-back-bs", on_back)
-- Media keys keep working through the input conf; the bar follows pause.
mp.observe_property("pause", "native", function(_, paused)
    if paused then show() elseif shown then arm_hide() end
    render()
end)
