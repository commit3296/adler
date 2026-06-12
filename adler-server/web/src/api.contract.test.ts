import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiClientError, api, streamScan } from "./api";
import type { CheckOutcome, FinishedScan, StartScanBody } from "./types";

const jsonHeaders = { "Content-Type": "application/json" };

function okJson(body: unknown): Response {
    return new Response(JSON.stringify(body), {
        status: 200,
        headers: jsonHeaders,
    });
}

function failJson(status: number, body: unknown, statusText = "Bad Request"): Response {
    return new Response(JSON.stringify(body), {
        status,
        statusText,
        headers: jsonHeaders,
    });
}

function jsonBody(init: RequestInit | undefined): unknown {
    expect(init?.body).toEqual(expect.any(String));
    return JSON.parse(init!.body as string);
}

function fullOutcome(): CheckOutcome {
    return {
        site: "GitHub",
        url: "https://github.com/alice",
        kind: "found",
        elapsed_ms: 11,
        evidence: ["HTTP 200 (status_found)"],
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
        ],
        confidence: {
            score: 95,
            label: "high",
            reasons: [
                { kind: "found_by_signal" },
                { kind: "signal_evidence", count: 1 },
                { kind: "profile_metadata_extracted", count: 1 },
                { kind: "browser_transport" },
                { kind: "escalated_transport" },
            ],
        },
        transport: "browser",
        escalations: 1,
    };
}

function finishedScan(): FinishedScan {
    const outcome = fullOutcome();
    return {
        summary: { found: 1, not_found: 0, uncertain: 0 },
        outcomes: [outcome],
        elapsed_ms: 25,
        identity_clusters: [
            {
                id: "identity-0001",
                confidence: 90,
                uncertain: false,
                reasons: [
                    {
                        kind: "shared_external_link",
                        value: "https://alice.dev/",
                    },
                ],
                members: [
                    {
                        site: "GitHub",
                        username: "alice",
                        url: "https://github.com/alice",
                        evidence: outcome.profile_evidence,
                        confidence: outcome.confidence!,
                        observed_at_ms: 1781192451000,
                    },
                ],
            },
        ],
    };
}

describe("adler-server HTTP API contract", () => {
    let fetchMock: ReturnType<typeof vi.fn>;

    beforeEach(() => {
        fetchMock = vi.fn().mockResolvedValue(okJson({ ok: true }));
        vi.stubGlobal("fetch", fetchMock);
    });

    afterEach(() => {
        vi.unstubAllGlobals();
    });

    it("maps read endpoints to the documented /api routes", async () => {
        fetchMock
            .mockResolvedValueOnce(okJson({ ok: true, version: "0.12.2" }))
            .mockResolvedValueOnce(okJson({ sites: [], disabled: [] }))
            .mockResolvedValueOnce(okJson({ egress: [], sessions: [] }))
            .mockResolvedValueOnce(okJson([]))
            .mockResolvedValueOnce(
                okJson({
                    from_scan_id: "old",
                    to_scan_id: "new",
                    added_found: [],
                    removed_found: [],
                    verdict_changes: [],
                    evidence_changes: [],
                }),
            )
            .mockResolvedValueOnce(
                okJson({
                    status: "running",
                    username: "alice",
                    site_count: 1,
                    elapsed_ms: 7,
                    partial: [],
                }),
            );

        await api.health();
        expect(await api.sites()).toEqual({ sites: [], disabled: [] });
        await api.access();
        await api.scans();
        await api.scanDiff("old", "new");
        await api.scan("scan_123");

        expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
            "/api/health",
            "/api/sites",
            "/api/access",
            "/api/scans",
            "/api/scans/old/diff/new",
            "/api/scan/scan_123",
        ]);
        expect(fetchMock.mock.calls.every(([, init]) => init === undefined)).toBe(
            true,
        );
    });

    it("accepts the legacy /api/sites array shape", async () => {
        fetchMock.mockResolvedValueOnce(
            okJson([{ name: "GitHub", url: "https://github.com/{username}", tags: [] }]),
        );

        await expect(api.sites()).resolves.toEqual({
            sites: [{ name: "GitHub", url: "https://github.com/{username}", tags: [] }],
            disabled: [],
        });
    });

    it("serializes write endpoints with JSON bodies accepted by adler-server", async () => {
        const scanBody: StartScanBody = {
            username: "alice",
            tag: ["coding"],
            exclude_tag: ["bot-protected"],
            top: 10,
            nsfw: false,
            concurrency: 4,
            deadline_secs: 15,
            egress_names: ["corp-de"],
        };

        fetchMock
            .mockResolvedValueOnce(
                okJson({ scan_id: "scan_1", username: "alice", site_count: 3 }),
            )
            .mockResolvedValueOnce(
                okJson({
                    outcome: {
                        site: "GitHub",
                        url: "https://github.com/alice",
                        kind: "found",
                        elapsed_ms: 11,
                    },
                }),
            )
            .mockResolvedValueOnce(
                okJson({
                    scan_id: "scan_2",
                    derived_from: "scan_1",
                    carried_outcomes: 1,
                    site_count: 2,
                }),
            );

        await api.startScan(scanBody);
        await api.retrySite("scan_1", "GitHub");
        await api.refilterScan("scan_1", {
            tag: ["social"],
            exclude: ["GitLab"],
            top: 5,
        });

        expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/scan", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(scanBody),
        });
        expect(fetchMock).toHaveBeenNthCalledWith(
            2,
            "/api/scan/scan_1/retry",
            expect.objectContaining({
                method: "POST",
                headers: { "Content-Type": "application/json" },
            }),
        );
        expect(jsonBody(fetchMock.mock.calls[1][1])).toEqual({ site: "GitHub" });
        expect(fetchMock).toHaveBeenNthCalledWith(
            3,
            "/api/scan/scan_1/refilter",
            expect.objectContaining({
                method: "POST",
                headers: { "Content-Type": "application/json" },
            }),
        );
        expect(jsonBody(fetchMock.mock.calls[2][1])).toEqual({
            tag: ["social"],
            exclude: ["GitLab"],
            top: 5,
        });
    });

    it("preserves server error envelopes as ApiClientError", async () => {
        fetchMock.mockResolvedValueOnce(
            failJson(404, {
                error: "scan_not_found",
                message: "scan does not exist",
            }),
        );

        await expect(api.scan("missing")).rejects.toMatchObject({
            name: "ApiClientError",
            code: "scan_not_found",
            message: "scan does not exist",
        } satisfies Partial<ApiClientError>);
    });

    it("preserves disabled matches on API errors", async () => {
        fetchMock.mockResolvedValueOnce(
            failJson(400, {
                error: "empty_site_filter",
                message: "no enabled sites match the requested filter",
                disabled_matches: [
                    {
                        name: "TikTok",
                        url: "https://www.tiktok.com/@{username}",
                        tags: ["social"],
                        disabled_reason: "Honest Limits",
                    },
                ],
            }),
        );

        const err = await api
            .startScan({ username: "alice", only: ["TikTok"] })
            .then(
                () => null,
                (e) => e,
            );

        expect(err).toBeInstanceOf(ApiClientError);
        expect(err.disabledMatches[0].name).toBe("TikTok");
    });

    it("accepts finished scan snapshots with evidence, confidence, and clusters", async () => {
        const finished = finishedScan();
        fetchMock.mockResolvedValueOnce(
            okJson({
                status: "finished",
                username: "alice",
                site_count: 1,
                ...finished,
            }),
        );

        const snapshot = await api.scan("scan_1");

        expect(snapshot.status).toBe("finished");
        if (snapshot.status !== "finished") throw new Error("expected finished scan");
        expect(snapshot.outcomes[0]).toEqual(finished.outcomes[0]);
        expect(snapshot.outcomes[0]?.profile_evidence?.[0]?.source.access_path).toEqual(
            {
                transport: "browser",
                escalated: true,
                authenticated: true,
            },
        );
        expect(snapshot.outcomes[0]?.confidence?.reasons).toContainEqual({
            kind: "escalated_transport",
        });
        expect(snapshot.identity_clusters?.[0]?.members[0]?.evidence?.[0]?.kind).toBe(
            "external_link",
        );
        expect(snapshot.identity_clusters?.[0]?.members[0]?.confidence.label).toBe(
            "high",
        );
    });

    it("preserves retry outcome evidence, confidence, and transport fields", async () => {
        const outcome = fullOutcome();
        fetchMock.mockResolvedValueOnce(okJson({ outcome }));

        const retry = await api.retrySite("scan_1", "GitHub");

        expect(retry.outcome).toEqual(outcome);
        expect(retry.outcome.profile_evidence?.[0]?.source.observed_at_ms).toBe(
            1781192451000,
        );
        expect(retry.outcome.confidence?.reasons).toContainEqual({
            kind: "browser_transport",
        });
        expect(retry.outcome.transport).toBe("browser");
        expect(retry.outcome.escalations).toBe(1);
    });
});

describe("adler-server SSE scan contract", () => {
    const instances: FakeEventSource[] = [];

    class FakeEventSource {
        url: string;
        closed = false;
        listeners = new Map<string, Array<(ev: MessageEvent) => void>>();

        constructor(url: string) {
            this.url = url;
            instances.push(this);
        }

        addEventListener(type: string, listener: (ev: MessageEvent) => void) {
            const existing = this.listeners.get(type) ?? [];
            this.listeners.set(type, [...existing, listener]);
        }

        emit(type: string, data: unknown) {
            const event = new MessageEvent(type, { data: JSON.stringify(data) });
            for (const listener of this.listeners.get(type) ?? []) listener(event);
        }

        close() {
            this.closed = true;
        }
    }

    beforeEach(() => {
        instances.length = 0;
        vi.stubGlobal("EventSource", FakeEventSource);
    });

    afterEach(() => {
        vi.unstubAllGlobals();
    });

    it("subscribes to /stream and maps start/outcome/done events", () => {
        const onStart = vi.fn();
        const onOutcome = vi.fn();
        const onDone = vi.fn();
        const close = streamScan("scan_1", { onStart, onOutcome, onDone });
        const source = instances[0]!;
        const outcome = fullOutcome();
        const finished = finishedScan();

        expect(source.url).toBe("/api/scan/scan_1/stream");
        source.emit("start", { username: "alice" });
        source.emit("outcome", outcome);
        source.emit("done", finished);

        expect(onStart).toHaveBeenCalledWith({ username: "alice" });
        expect(onOutcome).toHaveBeenCalledWith(outcome);
        expect(onDone).toHaveBeenCalledWith(finished);
        const done = onDone.mock.calls[0]?.[0];
        expect(
            done?.identity_clusters?.[0]?.members[0]?.evidence?.[0]?.source
                .access_path?.transport,
        ).toBe("browser");
        expect(source.closed).toBe(true);

        source.closed = false;
        close();
        expect(source.closed).toBe(true);
    });
});
