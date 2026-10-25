-- PGM-IoT Per-Substation Data Loading Script
-- Populates substation, sensor, measurement, alert for each substation ID in `ids`.

local SENSORS_PER_SS = const.sensors_per_substation       -- 100
local MEASUREMENTS_PER_SENSOR = const.initial_measurements_per_sensor  -- 1000
local BATCH_SIZE = 500

local SENSOR_TYPES = {
    {"VOLTAGE", "V"}, {"CURRENT", "A"}, {"POWER", "kW"}, {"TEMPERATURE", "C"},
    {"FREQUENCY", "Hz"}, {"PRESSURE", "PSI"}, {"HUMIDITY", "%"}, {"VIBRATION", "mm/s"},
}

-- Fetch base timestamps once via SQL, then compute offsets in pure Lua
local now_ts = query_one("SELECT EXTRACT(EPOCH FROM NOW())::bigint")
local base_24h = now_ts - 86400   -- 24 hours ago
local base_12h = now_ts - 43200   -- 12 hours ago

-- Epoch seconds -> "YYYY-MM-DD HH:MM:SS" (UTC), computed in pure Lua so the
-- COPY data needs no per-row SQL.
local function fmt_ts(epoch)
    -- Days since epoch
    local days = math.floor(epoch / 86400)
    local rem = epoch % 86400
    local hh = math.floor(rem / 3600)
    local mm = math.floor((rem % 3600) / 60)
    local ss = rem % 60

    -- Convert days since 1970-01-01 to Y-M-D using a well-known algorithm
    -- (Howard Hinnant's civil_from_days)
    local z = days + 719468
    local era = math.floor(z / 146097)
    local doe = z - era * 146097
    local yoe = math.floor((doe - math.floor(doe / 1460) + math.floor(doe / 36524) - math.floor(doe / 146096)) / 365)
    local y = yoe + era * 400
    local doy = doe - (365 * yoe + math.floor(yoe / 4) - math.floor(yoe / 100))
    local mp = math.floor((5 * doy + 2) / 153)
    local dd = doy - math.floor((153 * mp + 2) / 5) + 1
    local mo = mp + (mp < 10 and 3 or -9)
    if mo <= 2 then y = y + 1 end

    return string.format("%04d-%02d-%02d %02d:%02d:%02d", y, mo, dd, hh, mm, ss)
end

for _, ss_id in ipairs(ids) do
    local sensor_base = (ss_id - 1) * SENSORS_PER_SS

    -- 1. Substation
    copy_start("COPY substation FROM stdin")
    copy_write(string.format("%d\tSubstation-%05d\tLocation-%s\t%.6f\t%.6f\t%d\tACTIVE\t%s\n",
        ss_id, ss_id, rand_string(8, 16),
        rand_float(-90.0, 90.0), rand_float(-180.0, 180.0),
        rand_int(1000, 9999), fmt_ts(now_ts)))
    copy_end()

    -- 2. Sensors (100 per substation)
    copy_start("COPY sensor FROM stdin")
    for offset = 1, SENSORS_PER_SS do
        local sn_id = sensor_base + offset
        local st = SENSOR_TYPES[((offset - 1) % #SENSOR_TYPES) + 1]
        copy_write(string.format("%d\t%d\t%s\t%s\t%.2f\t%.2f\tACTIVE\t%.4f\t%s\t%s\n",
            sn_id, ss_id, st[1], st[2],
            rand_float(10.0, 50.0), rand_float(150.0, 200.0),
            rand_float(50.0, 150.0), fmt_ts(now_ts), fmt_ts(now_ts)))
    end
    copy_end()

    -- 3. Measurements (1000 per sensor = 100,000 per substation)
    local total_measurements = SENSORS_PER_SS * MEASUREMENTS_PER_SENSOR
    local n_batches = math.ceil(total_measurements / BATCH_SIZE)
    local m_id = ((ss_id - 1) * SENSORS_PER_SS * MEASUREMENTS_PER_SENSOR) + 1
    local sensor_offset = 1
    local meas_offset = 0

    for b = 1, n_batches do
        copy_start("COPY measurement FROM stdin")
        local batch_start = (b - 1) * BATCH_SIZE + 1
        local batch_end = math.min(b * BATCH_SIZE, total_measurements)

        for _ = batch_start, batch_end do
            local sn_id = sensor_base + sensor_offset
            local m_ts = fmt_ts(base_24h + meas_offset * 60)

            copy_write(string.format("%d\t%d\t%d\t%s\t%.4f\t%d\n",
                m_id, sn_id, ss_id, m_ts,
                rand_float(50.0, 150.0), rand_int(90, 99)))

            m_id = m_id + 1
            meas_offset = meas_offset + 1
            if meas_offset >= MEASUREMENTS_PER_SENSOR then
                sensor_offset = sensor_offset + 1
                meas_offset = 0
            end
        end
        copy_end()
    end

    -- 4. Alerts (~10% of sensors = 10 per substation)
    local alerts_per_ss = SENSORS_PER_SS / 10
    local a_id = ((ss_id - 1) * alerts_per_ss) + 1

    copy_start("COPY alert FROM stdin NULL AS ''")
    for i = 0, alerts_per_ss - 1 do
        local sn_id = sensor_base + (i * 10) + 1
        local a_epoch = base_12h + i * 1800  -- 30 minute intervals
        local a_ts = fmt_ts(a_epoch)
        local alert_type, value, threshold
        if rand_int(0, 1) == 1 then
            alert_type = "HIGH"
            value = rand_float(150.0, 200.0)
            threshold = 150.0
        else
            alert_type = "LOW"
            value = rand_float(0.0, 10.0)
            threshold = 10.0
        end
        local acknowledged = rand_int(1, 10) <= 3
        local ack_at = ""
        if acknowledged then
            ack_at = fmt_ts(a_epoch + 3600)  -- +1 hour
        end

        copy_write(string.format("%d\t%d\t%d\t%s\t%s\t%.4f\t%.2f\t%s\t%s\n",
            a_id, sn_id, ss_id, a_ts, alert_type, value, threshold,
            tostring(acknowledged), ack_at))
        a_id = a_id + 1
    end
    copy_end()
end
