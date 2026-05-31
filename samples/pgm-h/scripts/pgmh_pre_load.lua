-- PGM-H Pre-Load Script
-- Populates region (5 rows) and nation (25 rows) reference tables.

local REGIONS = {"AFRICA", "AMERICA", "ASIA", "EUROPE", "MIDDLE EAST"}
local NATIONS = {
    {"ALGERIA", 0}, {"ARGENTINA", 1}, {"BRAZIL", 1}, {"CANADA", 1}, {"EGYPT", 4},
    {"ETHIOPIA", 0}, {"FRANCE", 3}, {"GERMANY", 3}, {"INDIA", 2}, {"INDONESIA", 2},
    {"IRAN", 4}, {"IRAQ", 4}, {"JAPAN", 2}, {"JORDAN", 4}, {"KENYA", 0},
    {"MOROCCO", 0}, {"MOZAMBIQUE", 0}, {"PERU", 1}, {"CHINA", 2}, {"ROMANIA", 3},
    {"SAUDI ARABIA", 4}, {"VIETNAM", 2}, {"RUSSIA", 3}, {"UNITED KINGDOM", 3}, {"UNITED STATES", 1},
}

-- Region
copy_start("COPY region FROM stdin")
for i, name in ipairs(REGIONS) do
    copy_write(string.format("%d\t%s\t%s\n", i - 1, name, rand_string(31, 115)))
end
copy_end()

-- Nation
copy_start("COPY nation FROM stdin")
for i, n in ipairs(NATIONS) do
    copy_write(string.format("%d\t%s\t%d\t%s\n", i - 1, n[1], n[2], rand_string(31, 114)))
end
copy_end()
