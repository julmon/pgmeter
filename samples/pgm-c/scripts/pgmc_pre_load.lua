-- PGM-C Pre-Load Script
-- Populates the item table (100,000 rows, shared across all warehouses).
-- Runs once before per-warehouse loading.

local N_ITEMS = 100000
local BATCH_SIZE = 500

local function make_original(s)
    -- 10% chance to embed "ORIGINAL" in the string
    if rand_int(1, 100) <= 10 and #s > 9 then
        local pos = rand_int(1, #s - 9)
        return string.sub(s, 1, pos - 1) .. "ORIGINAL" .. string.sub(s, pos + 9)
    end
    return s
end

local n_batches = math.ceil(N_ITEMS / BATCH_SIZE)

for b = 1, n_batches do
    copy_start("COPY item FROM stdin")

    local batch_start = (b - 1) * BATCH_SIZE + 1
    local batch_end = math.min(b * BATCH_SIZE, N_ITEMS)

    for i_id = batch_start, batch_end do
        local i_name = rand_string(14, 24)
        local i_price = rand_float(1.00, 100.00)
        local i_im_id = rand_int(1, 10000)
        local i_data = make_original(rand_string(26, 50))

        copy_write(string.format("%d\t%d\t%s\t%.2f\t%s\n",
            i_id, i_im_id, i_name, i_price, i_data))
    end

    copy_end()
end
