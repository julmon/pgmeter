-- TPCx-IoT Data Ingestion Transaction
-- Bulk insert of sensor measurements (100 readings per transaction)

local substation_id = rand_int(min_id, max_id)
local sensor_base = (substation_id - 1) * const.sensors_per_substation
local sensor_id = sensor_base + rand_int(1, const.sensors_per_substation)

-- Pre-generate measurement data
local offsets = {}
local values = {}
local qualities = {}
for i = 0, const.measurement_batch_size - 1 do
    offsets[#offsets + 1] = tostring(i * 10)
    values[#values + 1] = string.format("%.4f", rand_float(45.0, 155.0))
    qualities[#qualities + 1] = tostring(rand_int(90, 99))
end

local last_reading = values[#values]
local create_alert = rand_int(1, 100) <= 10
local alert_value = rand_float(150.0, 200.0)

begin_tx()

-- Bulk insert using UNNEST
exec(
    "INSERT INTO measurement (m_id, m_sn_id, m_ss_id, m_timestamp, m_value, m_quality) "
    .. "SELECT nextval('measurement_id_seq'), "
    .. sensor_id .. ", " .. substation_id
    .. ", NOW() + (ms_offset * interval '1 millisecond'), value, quality "
    .. "FROM UNNEST(ARRAY[" .. table.concat(offsets, ",") .. "]::int[], "
    .. "ARRAY[" .. table.concat(values, ",") .. "]::float8[], "
    .. "ARRAY[" .. table.concat(qualities, ",") .. "]::int[]) AS t(ms_offset, value, quality)"
)

-- Update sensor last reading
exec(
    "UPDATE sensor SET sn_last_reading = $1, sn_last_reading_at = NOW() WHERE sn_id = $2",
    last_reading, sensor_id
)

-- Create alert if threshold violation (10% chance)
if create_alert then
    exec(
        "INSERT INTO alert (a_id, a_sn_id, a_ss_id, a_timestamp, a_type, a_value, a_threshold, a_acknowledged, a_acknowledged_at) "
        .. "VALUES (nextval('alert_id_seq'), $1, $2, NOW(), 'HIGH', $3, 150.0, false, NULL)",
        sensor_id, substation_id, alert_value
    )
end

commit()
