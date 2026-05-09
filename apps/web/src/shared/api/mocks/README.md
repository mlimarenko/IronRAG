# MSW mocks

Single canonical source of HTTP mocks for the IronRAG frontend, generated
from the OpenAPI contract.

## Layout

- `handlers.ts` — auto-generated. One default `http.<method>` per
  `(method, path)` from `apps/api/contracts/openapi.gen.yaml`. Returns
  `HttpResponse.json({}, { status: 200 })`.
- `server.ts` — Node `setupServer(...handlers)`. Lifecycle wired in
  `src/shared/test/setup.ts` (`beforeAll`, `afterEach`, `afterAll`).
- `browser.ts` — Browser `setupWorker(...handlers)` for Storybook /
  dev experiments. Not pulled into the production bundle.

## When does MSW intercept?

The unhandled-request policy is `bypass`. That means:
- Tests that already use `vi.mock('@/shared/api', ...)` or
  `vi.spyOn(Tag, 'method')` keep winning — MSW only ever sees requests
  that actually hit the network.
- A test that does NOT mock the SDK gets the seed `{}` response from
  `handlers.ts`. Override per-test with:

  ```ts
  import { http, HttpResponse } from "msw";
  import { opsLibraryDashboard } from "@/shared/api/mocks/fixtures";
  import { server } from "@/shared/api/mocks/server";

  it("renders the dashboard", () => {
    server.use(
      http.get("/v1/ops/libraries/:libraryId/dashboard", () =>
        HttpResponse.json(opsLibraryDashboard()),
      ),
    );
    // ...
  });
  ```

## Reusable fixtures

Realistic synthetic fixtures for the highest-traffic endpoints live in
`fixtures/`. Import them only from tests or Storybook stories when a
scenario needs a non-empty response:

```ts
import { http, HttpResponse } from "msw";
import { iamSession } from "@/shared/api/mocks/fixtures";
import { server } from "@/shared/api/mocks/server";

server.use(http.get("/v1/iam/session", () => HttpResponse.json(iamSession())));
```

## Regeneration

After `make backend-emit-openapi` regenerates the contract, run:

```bash
make frontend-mocks-regen
```

Do not edit `handlers.ts` by hand — it gets clobbered on the next
regeneration.
