-- PGM-C shared helpers, loaded once per VM through `scripts.modules` and
-- visible to the loaders (pgmc_pre_load.lua, pgmc_load.lua) and to every
-- transaction script.

-- TPC-C 2.1.6: non-uniform random numbers. NURand(A, x, y) =
-- (((random(0, A) | random(x, y)) + C) % (y - x + 1)) + x, with A = 255 for
-- customer last names, 1023 for customer ids and 8191 for item ids. The C
-- constants are fixed here; the run-time C for last names differs from the
-- load-time one by 88, within the 65..119 the specification requires.
C_LAST_LOAD, C_LAST_RUN, C_ID, C_ITEM = 42, 130, 719, 3815

function nurand(A, C, x, y)
    return (bit32.bor(rand_int(0, A), rand_int(x, y)) + C) % (y - x + 1) + x
end

-- Customer last names: the TPC-C syllable scheme, 1000 distinct names for
-- n = 0..999. The loader passes c_id - 1 for the first 1000 customers of a
-- district and a load-time NURand for the others (TPC-C 4.3.3.1).
local syllables = {"BAR", "OUGHT", "ABLE", "PRIS", "PRES", "ESE", "ANTI", "CALLY", "ATION", "EING"}

function gen_last(n)
    if n >= 1000 then n = nurand(255, C_LAST_LOAD, 0, 999) end
    local s = string.format("%03d", n)
    return syllables[tonumber(string.sub(s, 1, 1)) + 1]
        .. syllables[tonumber(string.sub(s, 2, 2)) + 1]
        .. syllables[tonumber(string.sub(s, 3, 3)) + 1]
end
