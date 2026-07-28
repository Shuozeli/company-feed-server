-- A structurally correct crawl does not prove content freshness when neither
-- the activating build nor any subsequent crawl has produced a publication
-- timestamp. Keep correctness independent, but stop reporting those recipes as
-- fresh until dated evidence is observed.

UPDATE company_news_recipe_state AS state
SET
    freshness_status = 'unknown',
    metadata = state.metadata || jsonb_build_object(
        'content_freshness_evidence', 'publication_date_unavailable',
        'freshness_policy', 'dated-evidence-v1'
    )
FROM company_news_recipes AS recipe
WHERE
    recipe.id = state.recipe_id
    AND recipe.status = 'active'
    AND state.correctness_status = 'passing'
    AND state.rebuild_required = false
    AND state.last_item_published_at IS NULL
    AND state.freshness_status = 'fresh';
