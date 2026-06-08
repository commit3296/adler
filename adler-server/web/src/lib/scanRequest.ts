import type { FilterState, ScanFilterSnapshot } from "../store";
import type { RefilterBody, StartScanBody } from "../types";

export function filterSnapshot(filter: FilterState): ScanFilterSnapshot {
    return {
        tag: [...filter.tag],
        excludeTag: [...filter.excludeTag],
        top: filter.top,
        nsfw: filter.nsfw,
        egressNames: [...filter.egressNames],
    };
}

export function scanRequestBody(
    username: string,
    filter: FilterState,
): StartScanBody {
    return {
        username,
        ...filterRequestBody(filter),
    };
}

export function refilterRequestBody(filter: FilterState): RefilterBody {
    return filterRequestBody(filter);
}

function filterRequestBody(filter: FilterState): RefilterBody {
    const body: RefilterBody = {};
    if (filter.tag.length) body.tag = filter.tag;
    if (filter.excludeTag.length) body.exclude_tag = filter.excludeTag;
    if (filter.top != null) body.top = filter.top;
    if (filter.nsfw) body.nsfw = true;
    if (filter.egressNames.length) body.egress_names = filter.egressNames;
    return body;
}
