create function atlas_par2_cache_header(r ext_raw_par2,direction bigint) returns void language plpgsql as $$
begin
 perform atlas_par2_cache_adjust(r.name,r.group_name,r.content_id,direction,atlas_par2_cache_size(r.name,r.group_name,r.content_id,coalesce(octet_length(r.natural_name_key),0)::bigint+coalesce(octet_length(r.natural_group_key),0)::bigint+coalesce(octet_length(r.posted),0)::bigint+coalesce(octet_length(r.content_fp),0)::bigint),0,r.natural_name_key,r.natural_group_key,''::bytea);
end $$;

create function atlas_par2_cache_files(r ext_raw_par2_files,direction bigint) returns void language plpgsql as $$
begin
 perform atlas_par2_cache_adjust(r.name,r.group_name,r.content_id,direction,atlas_par2_cache_size(r.name,r.group_name,r.content_id,coalesce(octet_length(r.natural_name_key),0)::bigint+coalesce(octet_length(r.natural_group_key),0)::bigint+coalesce(octet_length(r.natural_file_key),0)::bigint+coalesce(octet_length(r.file_id),0)::bigint+coalesce(octet_length(r.set_id),0)::bigint+coalesce(octet_length(r.md5),0)::bigint+coalesce(octet_length(r.md5_16k),0)::bigint+coalesce(octet_length(r.file_name),0)::bigint+coalesce(octet_length(r.creator),0)::bigint),1,r.natural_name_key,r.natural_group_key,r.natural_file_key);
end $$;

create function atlas_par2_cache_preserved(r ext_preserved_cache_rows,direction bigint) returns void language plpgsql as $$
begin
 if r.family not in ('ext_raw_par2','ext_raw_par2_files') then return; end if;
 perform atlas_par2_cache_adjust(r.name,r.group_name,r.content_id,direction,atlas_par2_cache_size(r.name,r.group_name,r.content_id,coalesce(octet_length(r.family),0)::bigint+coalesce(octet_length(r.source_digest),0)::bigint+coalesce(octet_length(r.row_digest),0)::bigint+coalesce(octet_length(r.natural_name_key),0)::bigint+coalesce(octet_length(r.natural_group_key),0)::bigint+coalesce(octet_length(r.schema_descriptor),0)::bigint+coalesce(octet_length(r.row_bytes),0)::bigint),case when r.family='ext_raw_par2' then 2 else 3 end,r.source_digest,r.row_digest,''::bytea);
end $$;

create function atlas_par2_cold_refuse() returns trigger language plpgsql as $$
begin raise exception 'archived cache scope is read-only' using errcode='55000';end $$;
revoke all on function atlas_par2_cold_refuse() from public;
revoke all on function atlas_par2_cache_header(ext_raw_par2,bigint),atlas_par2_cache_files(ext_raw_par2_files,bigint),atlas_par2_cache_preserved(ext_preserved_cache_rows,bigint) from public;
