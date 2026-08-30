// 診断専用: CEF が渡す dmabuf を Vulkan で import して中身を読む。
//
// GL (EGLImage) と CPU (mmap) の両経路で全ゼロにしか見えないバッファが、
// 書き込んだ本人と同じ Vulkan からなら見えるのかを判定する。
//   - 見える  → クライアント取り込みを Vulkan 化すれば解決できる
//   - 見えない → CEF/Chromium 側が書いていないことが確定する (上流問題)
//
// 恒久コードではないので、エラー処理は「失敗段階を返して抜ける」だけに絞る。

#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

// 戻り値: 0 = 成功 (out_rgba と out_non_zero が有効)、負 = 失敗段階。
int dmabuf_vulkan_probe_read(int dmabuf_file_descriptor, uint32_t width, uint32_t height,
                             uint32_t stride, uint64_t modifier,
                             unsigned char *out_rgba, uint64_t *out_non_zero) {
    VkInstance instance = VK_NULL_HANDLE;
    VkApplicationInfo application_info = {VK_STRUCTURE_TYPE_APPLICATION_INFO};
    application_info.apiVersion = VK_API_VERSION_1_1;
    VkInstanceCreateInfo instance_info = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};
    instance_info.pApplicationInfo = &application_info;
    if (vkCreateInstance(&instance_info, NULL, &instance) != VK_SUCCESS) {
        return -1;
    }

    uint32_t physical_count = 0;
    vkEnumeratePhysicalDevices(instance, &physical_count, NULL);
    if (physical_count == 0) {
        return -2;
    }
    VkPhysicalDevice physicals[8];
    if (physical_count > 8) physical_count = 8;
    vkEnumeratePhysicalDevices(instance, &physical_count, physicals);
    // ディスクリート GPU を優先する (llvmpipe を掴まないため)。
    VkPhysicalDevice physical = physicals[0];
    for (uint32_t index = 0; index < physical_count; index++) {
        VkPhysicalDeviceProperties properties;
        vkGetPhysicalDeviceProperties(physicals[index], &properties);
        if (properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU) {
            physical = physicals[index];
            break;
        }
    }

    uint32_t queue_family_count = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &queue_family_count, NULL);
    VkQueueFamilyProperties queue_families[16];
    if (queue_family_count > 16) queue_family_count = 16;
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &queue_family_count, queue_families);
    uint32_t queue_family = 0;
    for (uint32_t index = 0; index < queue_family_count; index++) {
        if (queue_families[index].queueFlags & VK_QUEUE_TRANSFER_BIT) {
            queue_family = index;
            break;
        }
    }

    const char *device_extensions[] = {
        "VK_KHR_external_memory",
        "VK_KHR_external_memory_fd",
        "VK_EXT_external_memory_dma_buf",
        "VK_EXT_image_drm_format_modifier",
        "VK_KHR_image_format_list",
        "VK_KHR_sampler_ycbcr_conversion",
        "VK_KHR_maintenance1",
        "VK_KHR_bind_memory2",
        "VK_KHR_get_memory_requirements2",
    };
    float queue_priority = 1.0f;
    VkDeviceQueueCreateInfo queue_info = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
    queue_info.queueFamilyIndex = queue_family;
    queue_info.queueCount = 1;
    queue_info.pQueuePriorities = &queue_priority;
    VkDeviceCreateInfo device_info = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO};
    device_info.queueCreateInfoCount = 1;
    device_info.pQueueCreateInfos = &queue_info;
    device_info.enabledExtensionCount = sizeof(device_extensions) / sizeof(device_extensions[0]);
    device_info.ppEnabledExtensionNames = device_extensions;
    VkDevice device = VK_NULL_HANDLE;
    if (vkCreateDevice(physical, &device_info, NULL, &device) != VK_SUCCESS) {
        return -3;
    }

    // dmabuf を DRM modifier 指定で VkImage として import する。
    VkSubresourceLayout plane_layout = {0};
    plane_layout.rowPitch = stride;
    VkImageDrmFormatModifierExplicitCreateInfoEXT modifier_info = {
        VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT};
    modifier_info.drmFormatModifier = modifier;
    modifier_info.drmFormatModifierPlaneCount = 1;
    modifier_info.pPlaneLayouts = &plane_layout;

    VkExternalMemoryImageCreateInfo external_info = {
        VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO};
    external_info.pNext = &modifier_info;
    external_info.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;

    VkImageCreateInfo image_info = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
    image_info.pNext = &external_info;
    image_info.imageType = VK_IMAGE_TYPE_2D;
    image_info.format = VK_FORMAT_B8G8R8A8_UNORM;
    image_info.extent.width = width;
    image_info.extent.height = height;
    image_info.extent.depth = 1;
    image_info.mipLevels = 1;
    image_info.arrayLayers = 1;
    image_info.samples = VK_SAMPLE_COUNT_1_BIT;
    image_info.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
    image_info.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    image_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    VkImage image = VK_NULL_HANDLE;
    VkResult image_result = vkCreateImage(device, &image_info, NULL, &image);
    if (image_result != VK_SUCCESS) {
        return -4;
    }

    VkMemoryRequirements requirements;
    vkGetImageMemoryRequirements(device, image, &requirements);

    // fd は import で所有権が移るため dup して渡す。
    int duplicated = dup(dmabuf_file_descriptor);
    VkImportMemoryFdInfoKHR import_info = {VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR};
    import_info.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
    import_info.fd = duplicated;
    VkMemoryDedicatedAllocateInfo dedicated_info = {
        VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO};
    dedicated_info.pNext = &import_info;
    dedicated_info.image = image;
    VkMemoryAllocateInfo allocate_info = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    allocate_info.pNext = &dedicated_info;
    allocate_info.allocationSize = requirements.size;
    // memoryTypeBits から最初に使えるものを選ぶ。
    uint32_t memory_type = 0;
    for (uint32_t index = 0; index < 32; index++) {
        if (requirements.memoryTypeBits & (1u << index)) {
            memory_type = index;
            break;
        }
    }
    allocate_info.memoryTypeIndex = memory_type;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    if (vkAllocateMemory(device, &allocate_info, NULL, &memory) != VK_SUCCESS) {
        close(duplicated);
        return -5;
    }
    if (vkBindImageMemory(device, image, memory, 0) != VK_SUCCESS) {
        return -6;
    }

    // 読み戻し先のホスト可視バッファ。
    VkBufferCreateInfo buffer_info = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
    buffer_info.size = (VkDeviceSize)width * height * 4;
    buffer_info.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    VkBuffer buffer = VK_NULL_HANDLE;
    if (vkCreateBuffer(device, &buffer_info, NULL, &buffer) != VK_SUCCESS) {
        return -7;
    }
    VkMemoryRequirements buffer_requirements;
    vkGetBufferMemoryRequirements(device, buffer, &buffer_requirements);
    VkPhysicalDeviceMemoryProperties memory_properties;
    vkGetPhysicalDeviceMemoryProperties(physical, &memory_properties);
    uint32_t host_type = UINT32_MAX;
    for (uint32_t index = 0; index < memory_properties.memoryTypeCount; index++) {
        if ((buffer_requirements.memoryTypeBits & (1u << index)) &&
            (memory_properties.memoryTypes[index].propertyFlags &
             (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)) ==
                (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)) {
            host_type = index;
            break;
        }
    }
    if (host_type == UINT32_MAX) {
        return -8;
    }
    VkMemoryAllocateInfo buffer_allocate = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    buffer_allocate.allocationSize = buffer_requirements.size;
    buffer_allocate.memoryTypeIndex = host_type;
    VkDeviceMemory buffer_memory = VK_NULL_HANDLE;
    if (vkAllocateMemory(device, &buffer_allocate, NULL, &buffer_memory) != VK_SUCCESS) {
        return -9;
    }
    vkBindBufferMemory(device, buffer, buffer_memory, 0);

    // コマンドバッファ: レイアウト遷移 → イメージ → バッファのコピー。
    VkCommandPoolCreateInfo pool_info = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
    pool_info.queueFamilyIndex = queue_family;
    VkCommandPool command_pool = VK_NULL_HANDLE;
    vkCreateCommandPool(device, &pool_info, NULL, &command_pool);
    VkCommandBufferAllocateInfo command_info = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
    command_info.commandPool = command_pool;
    command_info.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    command_info.commandBufferCount = 1;
    VkCommandBuffer command_buffer = VK_NULL_HANDLE;
    vkAllocateCommandBuffers(device, &command_info, &command_buffer);

    VkCommandBufferBeginInfo begin_info = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    vkBeginCommandBuffer(command_buffer, &begin_info);

    VkImageMemoryBarrier barrier = {VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER};
    barrier.srcAccessMask = 0;
    barrier.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
    barrier.oldLayout = VK_IMAGE_LAYOUT_UNDEFINED; // 外部の内容は GENERAL/UNDEFINED 扱いで取得
    barrier.newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    barrier.srcQueueFamilyIndex = VK_QUEUE_FAMILY_EXTERNAL;
    barrier.dstQueueFamilyIndex = queue_family;
    barrier.image = image;
    barrier.subresourceRange.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
    barrier.subresourceRange.levelCount = 1;
    barrier.subresourceRange.layerCount = 1;
    vkCmdPipelineBarrier(command_buffer, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                         VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);

    VkBufferImageCopy copy_region = {0};
    copy_region.imageSubresource.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
    copy_region.imageSubresource.layerCount = 1;
    copy_region.imageExtent.width = width;
    copy_region.imageExtent.height = height;
    copy_region.imageExtent.depth = 1;
    vkCmdCopyImageToBuffer(command_buffer, image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buffer,
                           1, &copy_region);
    vkEndCommandBuffer(command_buffer);

    VkQueue queue = VK_NULL_HANDLE;
    vkGetDeviceQueue(device, queue_family, 0, &queue);
    VkSubmitInfo submit_info = {VK_STRUCTURE_TYPE_SUBMIT_INFO};
    submit_info.commandBufferCount = 1;
    submit_info.pCommandBuffers = &command_buffer;
    if (vkQueueSubmit(queue, 1, &submit_info, VK_NULL_HANDLE) != VK_SUCCESS) {
        return -10;
    }
    vkQueueWaitIdle(queue);

    void *mapped = NULL;
    if (vkMapMemory(device, buffer_memory, 0, VK_WHOLE_SIZE, 0, &mapped) != VK_SUCCESS) {
        return -11;
    }
    const unsigned char *pixels = (const unsigned char *)mapped;
    size_t center = ((size_t)(height / 2) * width + width / 2) * 4;
    out_rgba[0] = pixels[center + 0];
    out_rgba[1] = pixels[center + 1];
    out_rgba[2] = pixels[center + 2];
    out_rgba[3] = pixels[center + 3];
    uint64_t non_zero = 0;
    size_t total = (size_t)width * height * 4;
    for (size_t index = 0; index < total; index++) {
        if (pixels[index] != 0) non_zero++;
    }
    *out_non_zero = non_zero;
    vkUnmapMemory(device, buffer_memory);

    // 診断なので後始末は device 破棄でまとめる。
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    return 0;
}
