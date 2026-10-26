-- PGM-DS Pre-Load Script (Phase 1)
-- Populates the scale-factor-INDEPENDENT dimension tables. These have fixed
-- cardinalities by definition (deterministic cross-products or fixed calendars),
-- so they load once here regardless of scale factor:
--
--   income_band             20 rows
--   ship_mode               20 rows
--   reason                  35 rows
--   household_demographics  7,200 rows   (cross product)
--   customer_demographics   1,920,800 rows (cross product)
--   date_dim                73,050 rows  (1900-01-01 .. 2100-01-01 inclusive)
--   time_dim                86,400 rows  (one row per second of day)
--
-- Surrogate keys are assigned sequentially from 1 (time_dim from 0) so the
-- scale-dependent dimensions and fact tables can reference them deterministically.
--
-- Fidelity note: this is "shaped" data -- correct cardinalities and realistic
-- categorical vocabularies so downstream queries filter meaningfully, but it is
-- NOT bit-identical to dsdgen output.

local BATCH = 50000

-- Pure-Lua Julian date arithmetic (shared convention with the fact loader).
local function ymd_to_days(y, m, d)
    local a = math.floor((14 - m) / 12)
    local y2 = y + 4800 - a
    local m2 = m + 12 * a - 3
    return d + math.floor((153 * m2 + 2) / 5) + 365 * y2
        + math.floor(y2 / 4) - math.floor(y2 / 100) + math.floor(y2 / 400) - 32045
end

local function days_to_ymd(jd)
    local a = jd + 32044
    local b = math.floor((4 * a + 3) / 146097)
    local c = a - math.floor(146097 * b / 4)
    local d = math.floor((4 * c + 3) / 1461)
    local e = c - math.floor(1461 * d / 4)
    local m = math.floor((5 * e + 2) / 153)
    local day = e - math.floor((153 * m + 2) / 5) + 1
    local month = m + 3 - 12 * math.floor(m / 10)
    local year = 100 * b + d - 4800 + math.floor(m / 10)
    return year, month, day
end

local function flush_copy(sql, t, n)
    if n == 0 then return end
    copy_start(sql)
    copy_write(table.concat(t, "", 1, n))
    copy_end()
end

local buf = {}

-- ============================================================
-- income_band (20 rows)
-- ============================================================
do
    local n = 0
    for i = 1, 20 do
        n = n + 1
        buf[n] = string.format("%d\t%d\t%d\n", i, (i - 1) * 10000, i * 10000 - 1)
    end
    flush_copy("COPY income_band FROM stdin", buf, n)
end

-- ============================================================
-- ship_mode (20 rows)
-- ============================================================
do
    local TYPES = {"EXPRESS", "NEXT DAY", "OVERNIGHT", "TWO DAY", "ONE DAY",
                   "LIBRARY", "SEA", "AIR", "REGULAR"}
    local CODES = {"AIR", "SURFACE", "SEA"}
    local CARRIERS = {"DHL", "UPS", "FEDEX", "USPS", "ZHOU", "MSC", "TBS",
                      "BOXBUNDLES", "GERMA", "ZOUROS", "ALLIANCE", "BARIAN",
                      "ORIENTAL", "UNITED", "HARMON", "GREAT EASTERN", "DIAMOND",
                      "AIRBORNE", "LATVIAN", "PRI"}
    local n = 0
    for i = 1, 20 do
        n = n + 1
        buf[n] = string.format("%d\tAAAAAAAA%08d\t%s\t%s\t%s\t%s\n",
            i, i,
            TYPES[((i - 1) % #TYPES) + 1],
            CODES[((i - 1) % #CODES) + 1],
            CARRIERS[i],
            string.format("%d", rand_int(1, 9999)))
    end
    flush_copy("COPY ship_mode FROM stdin", buf, n)
end

-- ============================================================
-- reason (35 rows)
-- ============================================================
do
    -- "reason N", the TPC-DS wording (q93 filters on r_reason_desc = 'reason 28').
    local n = 0
    for i = 1, 35 do
        n = n + 1
        buf[n] = string.format("%d\tAAAAAAAA%08d\treason %d\n", i, i, i)
    end
    flush_copy("COPY reason FROM stdin", buf, n)
end

-- ============================================================
-- household_demographics (7,200 rows = 20 x 6 x 10 x 6)
-- ============================================================
do
    local BUY = {"0-500", "501-1000", "1001-5000", "5001-10000", ">10000", "Unknown"}
    local sk = 0
    local n = 0
    for ib = 1, 20 do
        for bp = 1, #BUY do
            for dep = 0, 9 do
                for veh = 0, 5 do
                    sk = sk + 1
                    n = n + 1
                    buf[n] = string.format("%d\t%d\t%s\t%d\t%d\n",
                        sk, ib, BUY[bp], dep, veh)
                end
            end
        end
    end
    flush_copy("COPY household_demographics FROM stdin", buf, n)
end

-- ============================================================
-- customer_demographics (1,920,800 rows = 2x5x7x20x4x7x7x7)
-- ============================================================
do
    local GENDER = {"M", "F"}
    local MARITAL = {"M", "S", "D", "W", "U"}
    local EDU = {"Primary", "Secondary", "College", "2 yr Degree",
                 "4 yr Degree", "Advanced Degree", "Unknown"}
    local CREDIT = {"Low Risk", "Good", "High Risk", "Unknown"}
    local sk = 0
    local n = 0
    for g = 1, #GENDER do
        for ms = 1, #MARITAL do
            for ed = 1, #EDU do
                for pe = 1, 20 do
                    for cr = 1, #CREDIT do
                        for dc = 0, 6 do
                            for dec_ = 0, 6 do
                                for dcc = 0, 6 do
                                    sk = sk + 1
                                    n = n + 1
                                    buf[n] = string.format(
                                        "%d\t%s\t%s\t%s\t%d\t%s\t%d\t%d\t%d\n",
                                        sk, GENDER[g], MARITAL[ms], EDU[ed],
                                        pe * 500, CREDIT[cr], dc, dec_, dcc)
                                    if n >= BATCH then
                                        flush_copy("COPY customer_demographics FROM stdin", buf, n)
                                        n = 0
                                    end
                                end
                            end
                        end
                    end
                end
            end
        end
    end
    flush_copy("COPY customer_demographics FROM stdin", buf, n)
end

-- ============================================================
-- date_dim (73,049 rows: 1900-01-01 .. 2100-01-01)
-- ============================================================
do
    local DAY_NAMES = {"Sunday", "Monday", "Tuesday", "Wednesday",
                       "Thursday", "Friday", "Saturday"}
    -- month*100+day for a small set of fixed-date US holidays
    local HOLIDAYS = {
        [101] = true, [214] = true, [704] = true,
        [1031] = true, [1111] = true, [1225] = true,
    }
    local MDAYS = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31}
    local function days_in_month(y, m)
        if m == 2 and (y % 4 == 0 and (y % 100 ~= 0 or y % 400 == 0)) then
            return 29
        end
        return MDAYS[m]
    end
    local base_jd = ymd_to_days(1900, 1, 1)
    local end_jd = ymd_to_days(2100, 1, 1)
    local n = 0
    for jd = base_jd, end_jd do
        local y, m, d = days_to_ymd(jd)
        local dow = (jd + 1) % 7                 -- 0 = Sunday
        local qoy = math.floor((m - 1) / 3) + 1
        local month_seq = (y - 1900) * 12 + (m - 1)
        local quarter_seq = (y - 1900) * 4 + (qoy - 1)
        local week_seq = math.floor((jd - base_jd) / 7) + 1
        local first_dom = ymd_to_days(y, m, 1)
        local last_dom = ymd_to_days(y, m, days_in_month(y, m))
        -- same day last quarter (3 months back, clamp day for short months)
        local lqy, lqm = y, m - 3
        if lqm < 1 then lqm = lqm + 12; lqy = lqy - 1 end
        local holiday = HOLIDAYS[m * 100 + d] and "Y" or "N"
        local weekend = (dow == 0 or dow == 6) and "Y" or "N"
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t%04d-%02d-%02d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%s\t%04dQ%d\t%s\t%s\tN\t%d\t%d\t%d\t%d\tN\tN\tN\tN\tN\n",
            jd, jd, y, m, d,
            month_seq, week_seq, quarter_seq, y, dow, m, d, qoy,
            y, quarter_seq, week_seq,
            DAY_NAMES[dow + 1], y, qoy,
            holiday, weekend,
            first_dom, last_dom,
            ymd_to_days(y - 1, m, math.min(d, 28)),
            ymd_to_days(lqy, lqm, math.min(d, 28)))
        if n >= BATCH then
            flush_copy("COPY date_dim FROM stdin", buf, n)
            n = 0
        end
    end
    flush_copy("COPY date_dim FROM stdin", buf, n)
end

-- ============================================================
-- time_dim (86,400 rows: one per second of day, sk 0..86399)
-- ============================================================
do
    local n = 0
    for sec = 0, 86399 do
        local hour = math.floor(sec / 3600)
        local minute = math.floor((sec % 3600) / 60)
        local second = sec % 60
        local am_pm = hour < 12 and "AM" or "PM"
        local shift
        if hour < 8 then shift = "third"
        elseif hour < 16 then shift = "first"
        else shift = "second" end
        local sub_shift
        if hour < 6 then sub_shift = "night"
        elseif hour < 9 then sub_shift = "breakfast"
        elseif hour < 12 then sub_shift = "morning"
        elseif hour < 14 then sub_shift = "noon"
        elseif hour < 18 then sub_shift = "afternoon"
        else sub_shift = "evening" end
        local meal
        if hour >= 6 and hour < 9 then meal = "breakfast"
        elseif hour >= 11 and hour < 14 then meal = "lunch"
        elseif hour >= 17 and hour < 20 then meal = "dinner"
        else meal = "" end
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t%d\t%d\t%d\t%d\t%s\t%s\t%s\t%s\n",
            sec, sec, sec, hour, minute, second, am_pm, shift, sub_shift, meal)
        if n >= BATCH then
            flush_copy("COPY time_dim FROM stdin", buf, n)
            n = 0
        end
    end
    flush_copy("COPY time_dim FROM stdin", buf, n)
end
