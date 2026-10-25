-- PGM-C shared helpers, loaded once per VM through `scripts.modules` and
-- visible to the loaders (pgmc_pre_load.lua, pgmc_load.lua) and to every
-- transaction script.

-- Customer last names: the TPC-C syllable scheme, 1000 distinct names for
-- n = 0..999 (an n >= 1000 is drawn at random, as the specification does).
local syllables = {"BAR", "OUGHT", "ABLE", "PRIS", "PRES", "ESE", "ANTI", "CALLY", "ATION", "EING"}

function gen_last(n)
    if n >= 1000 then n = rand_int(0, 999) end
    local s = string.format("%03d", n)
    return syllables[tonumber(string.sub(s, 1, 1)) + 1]
        .. syllables[tonumber(string.sub(s, 2, 2)) + 1]
        .. syllables[tonumber(string.sub(s, 3, 3)) + 1]
end
