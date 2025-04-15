CREATE TABLE manager (
    id INTEGER PRIMARY KEY NOT NULL,
    active_game_slug TEXT
);

CREATE TABLE managed_games (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    slug TEXT NOT NULL,
    favorite BOOLEAN NOT NULL DEFAULT FALSE,
    active_profile_id INT NOT NULL
);

CREATE TABLE profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    game_slug TEXT NOT NULL,
    mods JSON NOT NULL,
    modpack JSON,
    ignored_updates JSON
);

CREATE TABLE prefs (
    id INTEGER PRIMARY KEY NOT NULL,
    data JSON NOT NULL
);

CREATE TABLE telemetry (
    id BLOB PRIMARY KEY NOT NULL,
    user_id UUID NOT NULL
);
