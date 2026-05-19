alter type web_boundary_policy add value if not exists 'same_host_and_subdomains';

alter type web_candidate_host_classification add value if not exists 'seed_subdomain';
