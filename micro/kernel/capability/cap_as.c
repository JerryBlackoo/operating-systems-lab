#include "cap_as.h"

#include "object.h"
#include "service_boot.h"

static int as_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    (void)obj;
    (void)op;
    (void)args;
    return CAP_E_TYPE;
}

static void as_finalize(struct cap_object *obj)
{
    (void)obj;
}

static const struct cap_object_ops as_ops = {
    .invoke = as_invoke,
    .finalize = as_finalize,
};

const struct cap_type cap_as_type = {
    .type = CAP_TYPE_AS,
    .rights_mask = CAP_RIGHT_MAP | CAP_RIGHT_GRANT,
    .supported = 1u << CAP_OP_MAP,
    .required = {[CAP_OP_MAP] = CAP_RIGHT_MAP},
    .ops = &as_ops,
};

void cap_as_register_type(void)
{
}

int cap_as_bind(struct as *as)
{
    cap_oid_t oid;

    if (as == 0 || !as->active) {
        return CAP_E_BAD_ARG;
    }
    if (as->cap_oid != 0) {
        return CAP_OK;
    }
    if (cap_object_create(&cap_as_type, as, &oid) != CAP_OK) {
        return CAP_E_NOMEM;
    }
    as->cap_oid = oid;
    return CAP_OK;
}

bool cap_vm_can_map_as(const struct as *as)
{
    if (as == 0 || as->cap_oid == 0) {
        return false;
    }
    return cap_holder_has_oid(SERVICE_SID_VM, (cap_oid_t)as->cap_oid,
                              CAP_RIGHT_MAP);
}

void cap_as_release(struct as *as)
{
    cap_oid_t oid;

    if (as == 0 || as->cap_oid == 0) {
        return;
    }
    oid = (cap_oid_t)as->cap_oid;
    as->cap_oid = 0;
    (void)cap_object_discard(oid);
}

int cap_pm_install_as_grant(struct as *as, cap_handle_t *out)
{
    if (as == 0 || out == 0) {
        return CAP_E_BAD_ARG;
    }
    if (cap_as_bind(as) != CAP_OK) {
        return CAP_E_NOMEM;
    }
    return cap_root_install(SERVICE_SID_PM, CAP_SLOT_AUTO,
                            (cap_oid_t)as->cap_oid,
                            CAP_RIGHT_MAP | CAP_RIGHT_GRANT,
                            (cap_badge_t)object_get_id(&as->obj), out);
}

int cap_pm_mint_vm_map(cap_sid_t pm_sid, cap_handle_t src, const struct as *as)
{
    cap_handle_t vm_h;

    if (as == 0 || as->cap_oid == 0) {
        return CAP_E_BAD_ARG;
    }
    if (cap_mint_to(pm_sid, src, SERVICE_SID_VM, CAP_SLOT_AUTO, CAP_RIGHT_MAP,
                    (cap_badge_t)object_get_id(&as->obj), &vm_h) != CAP_OK) {
        return CAP_E_RIGHTS;
    }
    /*
     * mint 成功后必须验证 VM 真的能用（同 oid + MAP 权）。若验证失败
     * 就把刚装进去的句柄删掉，否则每次失败都会在 VM CSpace 里留一个
     * 不可用的 cap，最终耗尽槽位。
     */
    if (!cap_vm_can_map_as(as)) {
        (void)cap_delete(SERVICE_SID_VM, vm_h);
        return CAP_E_RIGHTS;
    }
    return CAP_OK;
}

int cap_vm_ensure_as_map(struct as *as)
{
    cap_handle_t h;

    if (as == 0) {
        return CAP_E_BAD_ARG;
    }
    if (cap_as_bind(as) != CAP_OK) {
        return CAP_E_NOMEM;
    }
    if (cap_vm_can_map_as(as)) {
        return CAP_OK;
    }
    if (cap_root_install(SERVICE_SID_VM, CAP_SLOT_AUTO,
                         (cap_oid_t)as->cap_oid, CAP_RIGHT_MAP,
                         (cap_badge_t)object_get_id(&as->obj), &h) != CAP_OK) {
        return CAP_E_NOMEM;
    }
    /*
     * 安装成功后仍要复核：若 VM 的 CSpace 已满导致实际没装上，
     * 必须让调用者看到失败（缺页会被拒绝），而不是静默返回成功。
     */
    if (!cap_vm_can_map_as(as)) {
        (void)cap_delete(SERVICE_SID_VM, h);
        return CAP_E_NOMEM;
    }
    return CAP_OK;
}