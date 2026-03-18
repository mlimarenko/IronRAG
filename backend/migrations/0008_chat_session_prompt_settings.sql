alter table chat_session
    add column if not exists system_prompt text,
    add column if not exists prompt_state text,
    add column if not exists preferred_mode text;

update chat_session
set system_prompt = coalesce(
        nullif(trim(system_prompt), ''),
        'Answer using knowledge from the active library only. Gather all required context from that knowledge base before answering. Find the documents needed to support the answer, and when fragment-level evidence is insufficient, request or use broader full-document context. If the active library does not contain enough grounded evidence, say that plainly instead of guessing.'
    ),
    preferred_mode = coalesce(nullif(trim(preferred_mode), ''), 'hybrid');

update chat_session
set prompt_state = case
    when system_prompt = 'Answer using knowledge from the active library only. Gather all required context from that knowledge base before answering. Find the documents needed to support the answer, and when fragment-level evidence is insufficient, request or use broader full-document context. If the active library does not contain enough grounded evidence, say that plainly instead of guessing.'
        then 'default'
    else 'customized'
end
where prompt_state is null or trim(prompt_state) = '';

alter table chat_session
    alter column system_prompt set not null,
    alter column system_prompt set default 'Answer using knowledge from the active library only. Gather all required context from that knowledge base before answering. Find the documents needed to support the answer, and when fragment-level evidence is insufficient, request or use broader full-document context. If the active library does not contain enough grounded evidence, say that plainly instead of guessing.',
    alter column prompt_state set not null,
    alter column prompt_state set default 'default',
    alter column preferred_mode set not null,
    alter column preferred_mode set default 'hybrid';
