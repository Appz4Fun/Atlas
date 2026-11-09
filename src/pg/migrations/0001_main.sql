-- What atlas.db holds today: groups and their cursors, the backfill chunk queue and its bookkeeping, the
-- spotnet queues, and the main meta counters. Times are unix seconds, as in SQLite.

create table groups (
    name            text primary key,
    live_cursor     bigint,
    backfill_cursor bigint,
    first_article   bigint,
    last_article    bigint,
    low_article     bigint,
    low_posted      bigint,
    high_article    bigint,
    high_posted     bigint,
    start_posted    bigint
);

create table meta (
    key   text primary key,
    value bigint
);

create table backfill_chunks (
    grp        text not null,
    day        bigint not null,
    state      bigint not null,
    server     text,
    claimed_at bigint,
    done_at    bigint,
    failures   bigint,
    failed_at  bigint,
    reread     bigint,
    primary key (grp, day)
);
create index idx_backfill_chunks_day_state on backfill_chunks (day, state);
create index idx_backfill_chunks_grp_state_day on backfill_chunks (grp, state, day desc, claimed_at);

create table backfill_deepest (
    grp          text primary key,
    server       text not null,
    oldest_at    bigint,
    runner_up_at bigint
);

create table backfill_media (
    grp      text primary key,
    releases bigint not null
);

create table backfill_speculative (
    grp           text primary key,
    runner_up_day bigint not null,
    reach         bigint not null,
    days          bigint not null,
    articles      bigint not null
);

create table backfill_sweep_gen (
    grp text primary key,
    gen bigint not null
);

create table backfill_sweeps (
    grp    text not null,
    server text not null,
    primary key (grp, server)
);

create table backfill_wanted (
    grp text not null,
    day bigint not null,
    at  bigint not null,
    primary key (grp, day)
);

create table spot_blacklist (spotter text primary key);

create table spot_comments (
    message_id text primary key,
    spot       text not null,
    at         bigint not null,
    rating     bigint not null default 0,
    poster     text,
    spotter_id text,
    state      bigint not null default 0,
    tries      bigint not null default 0,
    body       text,
    seen       bigint not null default 0
);
create index spot_comments_spot on spot_comments (spot);
create index spot_comments_waiting on spot_comments (at) where state = 0;

create table spot_disposals (
    message_id text not null,
    spotter    text not null default '',
    at         bigint not null,
    primary key (message_id, spotter)
);

create table spot_reports (
    message_id text primary key,
    spot       text not null,
    at         bigint not null,
    poster     text,
    seen       bigint not null default 0
);
create index spot_reports_spot on spot_reports (spot);

create table spots_seen (
    message_id text primary key,
    number     bigint not null,
    state      bigint not null default 0,
    tries      bigint not null default 0,
    release_id bigint,
    at         bigint not null,
    spotter    text,
    nzb_ref    text,
    nzb_key    text
);
create index spots_seen_key on spots_seen (nzb_key) where nzb_key is not null;
create index spots_seen_ref on spots_seen (nzb_ref) where nzb_ref is not null;
create index spots_seen_release on spots_seen (release_id) where release_id is not null;
create index spots_seen_todo on spots_seen (state, number);

-- parts asked of the servers for a release's own ids (src/inferred.rs)
create table inferred_asked (
    release_id bigint primary key,
    next_at    bigint not null
);
create table inferred_ids (
    message_id text primary key,
    next_at    bigint not null,
    tries      bigint not null
);

-- posts the repair pass has tried (src/repair.rs)
create table repair_tried (
    release_id bigint primary key,
    at         bigint not null,
    permille   bigint
);
create table repair_older (
    shard  bigint primary key,
    before bigint
);

-- the id counter starts where SQLite's does (store::create_main)
insert into meta (key, value) values ('next_seq', 1);
