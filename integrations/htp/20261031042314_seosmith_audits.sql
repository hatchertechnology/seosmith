-- Seosmith pushes immutable reports. Provision a separate secret for each website.
create table public.seosmith_bindings (
  website_id uuid primary key references public.websites(id) on delete cascade,
  token_sha256 text not null unique check (token_sha256 ~ '^[a-f0-9]{64}$'),
  generation bigint not null default 1 check (generation > 0),
  enabled boolean not null default true
);
create table public.seosmith_audits (
  delivery_id uuid primary key,
  website_id uuid not null references public.websites(id) on delete cascade,
  fqdn text not null,
  generation bigint not null,
  sha256 text not null check (sha256 ~ '^[a-f0-9]{64}$'),
  received_at timestamptz not null default now(),
  report jsonb not null
);
create index seosmith_audits_website_received on public.seosmith_audits(website_id,received_at desc,delivery_id);
alter table public.seosmith_bindings enable row level security;
alter table public.seosmith_audits enable row level security;
revoke all on public.seosmith_bindings, public.seosmith_audits from public,anon,authenticated;
grant all on public.seosmith_bindings,public.seosmith_audits to service_role;

create function public.ingest_seosmith_audit(p_token text,p_delivery_id uuid,p_sha256 text,p_report_json text)
returns jsonb language plpgsql security definer set search_path = '' as $$
declare b public.seosmith_bindings; w public.websites; r jsonb; old public.seosmith_audits;
begin
  if length(p_token)<32 or octet_length(p_report_json)>16777216 or
     p_sha256 is distinct from encode(sha256(convert_to(p_report_json,'UTF8')),'hex') then
    raise exception 'invalid_delivery' using errcode='22023';
  end if;
  select * into b from public.seosmith_bindings where token_sha256=encode(sha256(convert_to(p_token,'UTF8')),'hex') and enabled for share;
  if not found then raise exception 'invalid_worker' using errcode='42501'; end if;
  select * into strict w from public.websites where id=b.website_id for share;
  r := p_report_json::jsonb;
  if r->>'schema_version' is distinct from '1' or r#>>'{request,request_id}' is distinct from p_delivery_id::text or
     r#>>'{request,website_id}' is distinct from b.website_id::text or
     r#>>'{request,binding_generation}' is distinct from b.generation::text or
     split_part(split_part(r#>>'{request,url}','://',2),'/',1) is distinct from w.fqdn or
     coalesce(r->>'outcome','') not in ('completed','partial','cancelled','failed') or
     jsonb_typeof(r->'pages') is distinct from 'array' or jsonb_typeof(r->'findings') is distinct from 'array' then
    raise exception 'invalid_binding_or_report' using errcode='22023';
  end if;
  if jsonb_array_length(r->'pages')>20000 or jsonb_array_length(r->'findings')>200000 then
    raise exception 'report_limit' using errcode='22023';
  end if;
  -- Serialize duplicate deliveries across transactions; acknowledge only identical content.
  perform pg_advisory_xact_lock(hashtextextended(p_delivery_id::text,0));
  select * into old from public.seosmith_audits where delivery_id=p_delivery_id;
  if found then
    if old.sha256<>p_sha256 or old.website_id<>b.website_id then raise exception 'delivery_conflict' using errcode='23505'; end if;
  else
    insert into public.seosmith_audits(delivery_id,website_id,fqdn,generation,sha256,report)
      values(p_delivery_id,b.website_id,w.fqdn,b.generation,p_sha256,r);
  end if;
  return jsonb_build_object('delivery_id',p_delivery_id,'sha256',p_sha256,'accepted',true);
end $$;
revoke all on function public.ingest_seosmith_audit(text,uuid,text,text) from public;
-- Credential-authenticated entry point: the independently provisioned website token
-- is checked inside the function; anonymous access without that secret cannot write.
grant execute on function public.ingest_seosmith_audit(text,uuid,text,text) to anon,service_role;

create function public.read_seosmith_audits(p_website_id uuid,p_limit integer default 10,p_before timestamptz default null,p_before_id uuid default null)
returns jsonb language plpgsql security definer set search_path = '' as $$
declare w public.websites;
begin
  select * into w from public.websites where id=p_website_id and owner_id=auth.uid();
  if not found or coalesce(auth.jwt()->>'is_anonymous','true')<>'false' then raise exception 'forbidden' using errcode='42501'; end if;
  if p_limit is null or p_limit<1 or p_limit>100 or (p_before is null)<>(p_before_id is null) then raise exception 'invalid_limit' using errcode='22023'; end if;
  return (select coalesce(jsonb_agg(to_jsonb(a) order by a.received_at desc,a.delivery_id),'[]'::jsonb) from
    (select delivery_id,received_at,report from public.seosmith_audits where website_id=w.id and fqdn=w.fqdn
      and (p_before is null or (received_at,delivery_id)<(p_before,p_before_id)) order by received_at desc,delivery_id limit p_limit) a);
end $$;
revoke all on function public.read_seosmith_audits(uuid,integer,timestamptz,uuid) from public;
grant execute on function public.read_seosmith_audits(uuid,integer,timestamptz,uuid) to authenticated;
