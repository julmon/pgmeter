-- TPCx-IoT Threshold Update Transaction
-- Updates sensor threshold settings

local substation_id = rand_int(min_id, max_id)
local sensor_base = (substation_id - 1) * const.sensors_per_substation
local sensor_id = sensor_base + rand_int(1, const.sensors_per_substation)

local min_threshold = rand_float(10.0, 50.0)
local max_threshold = rand_float(150.0, 200.0)

begin()

exec(
    "UPDATE sensor SET sn_min_threshold = $1, sn_max_threshold = $2 WHERE sn_id = $3",
    min_threshold, max_threshold, sensor_id
)

commit()
