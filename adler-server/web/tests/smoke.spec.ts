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
                site_count: 2,
                summary: { found: 1, not_found: 1, uncertain: 0 },
                elapsed_ms: 64,
                outcomes: [
                    {
                        site: "GitHub",
                        url: "https://github.com/alice",
                        kind: "found",
                        elapsed_ms: 12,
                        transport: "http",
                    },
                    {
                        site: "Example Social",
                        url: "https://social.example/alice",
                        kind: "not_found",
                        elapsed_ms: 52,
                        reason: "username_not_allowed",
                        transport: "http",
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
    await expect(page.getByRole("main").getByText("GitHub", { exact: true })).toBeVisible();
    await expect(
        page.getByRole("link", { name: "https://github.com/alice" }),
    ).toBeVisible();
    await expect(page.getByText("1 not_found hidden")).toBeVisible();
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
