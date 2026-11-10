-- Tokens outlive queue rows so a later claim can never match an old worker's token.
create sequence pp_queue_lease_token_seq as bigint;
alter table pp_queue
    add column lease_token bigint,
    add column lease_until bigint,
    add constraint pp_queue_lease_pair check ((lease_token is null) = (lease_until is null));
