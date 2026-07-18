-- One child content-mutation item belongs to exactly one discovered web page.
-- Besides enforcing lifecycle ownership, this index keeps post-publication
-- settlement lookup bounded as a web-run corpus grows.
create unique index if not exists idx_content_web_discovered_page_mutation_item
    on public.content_web_discovered_page (mutation_item_id)
    where mutation_item_id is not null;
