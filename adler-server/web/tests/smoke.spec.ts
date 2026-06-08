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
