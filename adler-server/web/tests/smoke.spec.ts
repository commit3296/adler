import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
    await page.route("/api/health", (route) =>
        route.fulfill({ json: { ok: true, version: "test" } }),
    );
    await page.route("/api/sites", (route) =>
        route.fulfill({
            json: [
                {
                    name: "GitHub",
                    url: "https://github.com/{username}",
                    tags: ["dev"],
                    popularity: 1,
                },
                {
                    name: "Example Social",
                    url: "https://social.example/{username}",
                    tags: ["social"],
                    popularity: 2,
                },
            ],
        }),
    );
    await page.route("/api/access", (route) =>
        route.fulfill({ json: { egress: [], sessions: [] } }),
    );
    await page.route("/api/scans", (route) => route.fulfill({ json: [] }));
    await page.route("/api/scans/old/diff/new", (route) =>
        route.fulfill({
            json: {
                from_scan_id: "old",
                to_scan_id: "new",
                added_found: [
                    {
                        site: "Example Social",
                        url: "https://social.example/alice",
                        kind: "found",
                        elapsed_ms: 31,
                    },
                ],
                removed_found: [],
                verdict_changes: [
                    {
                        site: "Example Social",
                        before: "not_found",
                        after: "found",
                    },
                ],
                evidence_changes: [],
            },
        }),
    );
    await page.route("/api/scan/old", (route) =>
        route.fulfill({
            json: {
                status: "finished",
                username: "alice",
                site_count: 2,
                summary: { found: 1, not_found: 1, uncertain: 0 },
                elapsed_ms: 50,
                outcomes: [
                    {
                        site: "GitHub",
                        url: "https://github.com/alice",
                        kind: "found",
                        elapsed_ms: 12,
                    },
                ],
            },
        }),
    );
    await page.route("/api/scan/new", (route) =>
        route.fulfill({
            json: {
                status: "finished",
                username: "alice",
                site_count: 2,
                summary: { found: 2, not_found: 0, uncertain: 0 },
                elapsed_ms: 63,
                outcomes: [
                    {
                        site: "GitHub",
                        url: "https://github.com/alice",
                        kind: "found",
                        elapsed_ms: 12,
                    },
                    {
                        site: "Example Social",
                        url: "https://social.example/alice",
                        kind: "found",
                        elapsed_ms: 31,
                    },
                ],
            },
        }),
    );
    await page.route("/api/scan/finished123", (route) =>
        route.fulfill({
            json: {
                status: "finished",
                username: "alice",
                site_count: 4,
                summary: { found: 2, not_found: 1, uncertain: 1 },
                elapsed_ms: 64,
                outcomes: [
                    {
                        site: "GitHub",
                        url: "https://github.com/alice",
                        kind: "found",
                        elapsed_ms: 12,
                        evidence: [
                            "HTTP 200 (status_found)",
                            "body matched profile marker",
                        ],
                        profile_evidence: [
                            {
                                kind: "external_link",
                                field: "website",
                                value: "https://alice.dev",
                                source: {
                                    site: "GitHub",
                                    url: "https://github.com/alice",
                                    origin: "extractor",
                                    observed_at_ms: 1781192451000,
                                    access_path: {
                                        transport: "browser",
                                        escalated: true,
                                        authenticated: true,
                                    },
                                },
                            },
                            {
                                kind: "display_name",
                                field: "name",
                                value: "Alice Example",
                                source: {
                                    site: "GitHub",
                                    url: "https://github.com/alice",
                                    origin: "extractor",
                                    observed_at_ms: 1781192451000,
                                    access_path: {
                                        transport: "browser",
                                        escalated: true,
                                        authenticated: true,
                                    },
                                },
                            },
                        ],
                        confidence: {
                            score: 85,
                            label: "high",
                            reasons: [
                                { kind: "found_by_signal" },
                                { kind: "signal_evidence", count: 2 },
                                { kind: "profile_metadata_extracted", count: 2 },
                                { kind: "authenticated_access" },
                                { kind: "browser_transport" },
                                { kind: "escalated_transport" },
                            ],
                        },
                        transport: "browser",
                        escalations: 1,
                    },
                    {
                        site: "Example Social",
                        url: "https://social.example/alice",
                        kind: "found",
                        elapsed_ms: 31,
                        evidence: ["HTTP 200 (status_found)"],
                        profile_evidence: [
                            {
                                kind: "external_link",
                                field: "website",
                                value: "https://alice.dev",
                                source: {
                                    site: "Example Social",
                                    url: "https://social.example/alice",
                                    origin: "extractor",
                                    observed_at_ms: 1781192452000,
                                },
                            },
                        ],
                        confidence: {
                            score: 65,
                            label: "medium",
                            reasons: [
                                { kind: "found_by_signal" },
                                { kind: "signal_evidence", count: 1 },
                            ],
                        },
                        transport: "http",
                    },
                    {
                        site: "Example Forum",
                        url: "https://forum.example/alice",
                        kind: "uncertain",
                        elapsed_ms: 44,
                        reason: "rate_limited",
                        confidence: {
                            score: 15,
                            label: "low",
                            reasons: [
                                { kind: "uncertain_outcome" },
                                { kind: "transport_blocked" },
                            ],
                        },
                        transport: "http",
                    },
                    {
                        site: "Example Archive",
                        url: "https://archive.example/alice",
                        kind: "not_found",
                        elapsed_ms: 52,
                        reason: "username_not_allowed",
                        transport: "http",
                    },
                ],
                identity_clusters: [
                    {
                        id: "identity-0001",
                        confidence: 90,
                        uncertain: true,
                        reasons: [
                            {
                                kind: "shared_external_link",
                                value: "https://alice.dev",
                            },
                        ],
                        members: [
                            {
                                site: "GitHub",
                                username: "alice",
                                url: "https://github.com/alice",
                                evidence: [
                                    {
                                        kind: "external_link",
                                        field: "website",
                                        value: "https://alice.dev",
                                        source: {
                                            site: "GitHub",
                                            url: "https://github.com/alice",
                                            origin: "extractor",
                                            observed_at_ms: 1781192451000,
                                        },
                                    },
                                ],
                                confidence: {
                                    score: 85,
                                    label: "high",
                                    reasons: [{ kind: "found_by_signal" }],
                                },
                                observed_at_ms: 1781192451000,
                            },
                            {
                                site: "Example Social",
                                username: "alice",
                                url: "https://social.example/alice",
                                evidence: [
                                    {
                                        kind: "external_link",
                                        field: "website",
                                        value: "https://alice.dev",
                                        source: {
                                            site: "Example Social",
                                            url: "https://social.example/alice",
                                            origin: "extractor",
                                            observed_at_ms: 1781192452000,
                                        },
                                    },
                                ],
                                confidence: {
                                    score: 65,
                                    label: "medium",
                                    reasons: [{ kind: "found_by_signal" }],
                                },
                                observed_at_ms: 1781192452000,
                            },
                        ],
                    },
                ],
            },
        }),
    );
});

test("home view loads catalogue and opens filters", async ({ page }) => {
    await page.goto("/");

    await expect(page.getByRole("link", { name: "ADLER" })).toBeVisible();
    await expect(page.getByPlaceholder("username")).toBeVisible();
    await expect(page.getByText("2 sites")).toBeVisible();

    await page.getByRole("button", { name: "Advanced filters" }).click();
    await expect(page.getByRole("dialog", { name: "Advanced filters" })).toBeVisible();
    await expect(page.getByRole("checkbox", { name: "dev 1" })).toBeVisible();
    await expect(page.getByRole("checkbox", { name: "social 1" })).toBeVisible();
});

test("routed finished scan renders snapshot results", async ({ page }) => {
    await page.goto("/#/scan/finished123");

    await expect(page.locator(".scan-username").getByText("alice")).toBeVisible();
    await expect(
        page.locator(".result-row", { hasText: "GitHub" }).getByText("GitHub", {
            exact: true,
        }),
    ).toBeVisible();
    await expect(
        page
            .locator(".result-row", { hasText: "GitHub" })
            .getByRole("link", { name: "https://github.com/alice" }),
    ).toBeVisible();
    await expect(page.getByText("1 not_found hidden")).toBeVisible();
});

test("routed finished scan renders confidence, evidence, and clusters", async ({
    page,
}) => {
    await page.goto("/#/scan/finished123");

    const high = page.getByTestId("confidence-chip").filter({ hasText: "high 85%" });
    const medium = page
        .getByTestId("confidence-chip")
        .filter({ hasText: "medium 65%" });
    const low = page.getByTestId("confidence-chip").filter({ hasText: "low 15%" });

    await expect(high).toBeVisible();
    await expect(high).toHaveClass(/confidence-high/);
    await expect(medium).toBeVisible();
    await expect(medium).toHaveClass(/confidence-medium/);
    await expect(low).toBeVisible();
    await expect(low).toHaveClass(/confidence-low/);

    await expect(page.getByTestId("transport-chip").filter({ hasText: "browser*" }))
        .toBeVisible();

    const githubRow = page.locator(".result-row", { hasText: "GitHub" });
    await githubRow.getByText("GitHub", { exact: true }).click();
    const drawer = githubRow.getByTestId("evidence-drawer");
    await expect(drawer).toBeVisible();
    await expect(drawer).toContainText("found by detection signal");
    await expect(drawer).toContainText("2 signal evidence lines recorded");
    await expect(drawer).toContainText("HTTP 200 (status_found)");
    await expect(drawer).toContainText("external_link (website): https://alice.dev");
    await expect(drawer).toContainText("display_name (name): Alice Example");

    const panel = page.getByTestId("identity-cluster-panel");
    await expect(panel).toBeVisible();
    const card = panel.getByTestId("identity-cluster-card");
    await expect(card).toContainText("identity-0001");
    await expect(card).toContainText("90%");
    await expect(card).toContainText("uncertain");
    await expect(card).toContainText("external link: https://alice.dev");
    await expect(card.getByRole("link", { name: /GitHub/ })).toHaveAttribute(
        "href",
        "https://github.com/alice",
    );
    await expect(
        card.getByRole("link", { name: /Example Social/ }),
    ).toHaveAttribute("href", "https://social.example/alice");
});

test("routed diff renders server scan diff details", async ({ page }) => {
    await page.goto("/#/diff/old/new");

    await expect(page.locator(".diff-summary").getByText("old")).toBeVisible();
    await expect(page.locator(".diff-summary").getByText("new")).toBeVisible();
    await expect(page.getByText("+ NEW")).toBeVisible();
    await expect(page.getByText("VERDICT CHANGED")).toBeVisible();
    await expect(
        page.getByRole("main").getByText("Example Social", { exact: true }).first(),
    ).toBeVisible();
    await expect(page.getByText("not_found")).toBeVisible();
    await expect(page.getByText("found").first()).toBeVisible();
});
